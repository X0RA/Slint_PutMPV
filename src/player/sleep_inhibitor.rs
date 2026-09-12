//! Keep the display awake during playback. All platform handles are created
//! and released on one worker thread (required by the Windows backend).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::linux_inhibitor::KeepAwake;
#[cfg(not(target_os = "linux"))]
use keepawake::KeepAwake;
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InhibitionKind {
    Display,
    #[cfg(target_os = "linux")]
    Idle,
}

pub struct SleepInhibitor {
    sender: Sender<bool>,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("playback-inhibitor".into())
            .spawn(move || {
                run_worker(receiver, acquire, Duration::from_secs(5));
            })
            .expect("could not start playback inhibitor worker");
        Self { sender }
    }

    pub fn acquire(&self) {
        let _ = self.sender.send(true);
    }

    pub fn release(&self) {
        let _ = self.sender.send(false);
    }
}

fn run_worker<T>(
    receiver: Receiver<bool>,
    mut acquire: impl FnMut(InhibitionKind, bool) -> Option<T>,
    retry_interval: Duration,
) {
    let mut active = false;
    let mut display = None;
    let mut display_attempted = false;
    #[cfg(target_os = "linux")]
    let mut idle_attempted = false;
    #[cfg(target_os = "linux")]
    let mut idle = None;
    loop {
        let missing = display.is_none();
        #[cfg(target_os = "linux")]
        let missing = missing || idle.is_none();
        // Wake on a timer only while a failed request actually needs retrying.
        let message = if active && missing {
            receiver.recv_timeout(retry_interval)
        } else {
            receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match message {
            Ok(value) => active = value,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Only the latest playback state matters during rapid toggles.
        for value in receiver.try_iter() {
            active = value;
        }
        if active {
            if display.is_none() {
                display = acquire(InhibitionKind::Display, !display_attempted);
                display_attempted = true;
            }
            // Linux's display and logind services can fail independently.
            // Keep whichever request succeeds and retry missing handles.
            #[cfg(target_os = "linux")]
            if idle.is_none() {
                idle = acquire(InhibitionKind::Idle, !idle_attempted);
                idle_attempted = true;
            }
        } else {
            #[cfg(target_os = "linux")]
            {
                idle = None;
                idle_attempted = false;
            }
            display = None;
            display_attempted = false;
        }
    }
}

fn acquire(kind: InhibitionKind, report_failure: bool) -> Option<KeepAwake> {
    #[cfg(target_os = "linux")]
    let result = KeepAwake::acquire(kind);
    #[cfg(not(target_os = "linux"))]
    let result = keepawake::Builder::default()
        .display(kind == InhibitionKind::Display)
        .idle(true)
        .reason("Video playback")
        .app_name("PutMPV")
        .app_reverse_domain("io.github.x0ra.putmpv")
        .create();
    match result {
        Ok(handle) => Some(handle),
        Err(e) => {
            if report_failure {
                warn!(
                    ?kind,
                    "could not acquire playback inhibitor; will retry quietly: {e:?}"
                );
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    struct Handle(Sender<thread::ThreadId>);
    impl Drop for Handle {
        fn drop(&mut self) {
            self.0.send(thread::current().id()).unwrap();
        }
    }

    #[test]
    fn pause_resume_and_shutdown_release_handles_on_the_owner_thread() {
        let (commands, receiver) = mpsc::channel();
        let (created, creations) = mpsc::channel();
        let (dropped, drops) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_worker(
                receiver,
                move |_, _| {
                    created.send(thread::current().id()).unwrap();
                    Some(Handle(dropped.clone()))
                },
                Duration::from_secs(5),
            )
        });
        let count = if cfg!(target_os = "linux") { 2 } else { 1 };
        for _ in 0..2 {
            commands.send(true).unwrap();
            for _ in 0..count {
                assert_eq!(
                    creations.recv_timeout(Duration::from_secs(2)).unwrap(),
                    worker.thread().id()
                );
            }
            commands.send(false).unwrap();
            for _ in 0..count {
                assert_eq!(
                    drops.recv_timeout(Duration::from_secs(2)).unwrap(),
                    worker.thread().id()
                );
            }
        }
        commands.send(true).unwrap();
        for _ in 0..count {
            creations.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        drop(commands);
        for _ in 0..count {
            assert_eq!(
                drops.recv_timeout(Duration::from_secs(2)).unwrap(),
                worker.thread().id()
            );
        }
        worker.join().unwrap();
    }

    #[test]
    fn failed_display_request_is_retried_without_another_resume() {
        let (commands, receiver) = mpsc::channel();
        let (attempted, attempts) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_worker(
                receiver,
                move |kind, report_failure| {
                    if kind == InhibitionKind::Display {
                        attempted.send(report_failure).unwrap();
                    }
                    None::<()>
                },
                Duration::from_millis(10),
            )
        });
        commands.send(true).unwrap();
        assert!(attempts.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!attempts.recv_timeout(Duration::from_secs(2)).unwrap());
        drop(commands);
        worker.join().unwrap();
    }
}
