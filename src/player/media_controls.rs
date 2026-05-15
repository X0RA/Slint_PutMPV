//! Cross-platform OS media controls (MPRIS / SMTC / MPRemoteCommandCenter)
//! wrapper around `souvlaki`. Registers the app as the active media player
//! only while playback is loaded; releases when playback ends.

use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tracing::warn;

/// High-level commands routed from the OS to the app.
#[derive(Debug, Clone, Copy)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    Seek(f64),
}

type EventCallback = Arc<dyn Fn(MediaCommand) + Send + Sync + 'static>;

pub struct MediaControlsWrapper {
    inner: Option<MediaControls>,
    hwnd: Option<usize>,
    on_event: EventCallback,
    paused: bool,
    position_secs: f64,
    last_pushed_position: f64,
}

impl MediaControlsWrapper {
    pub fn new<F>(hwnd: Option<usize>, on_event: F) -> Self
    where
        F: Fn(MediaCommand) + Send + Sync + 'static,
    {
        Self {
            inner: None,
            hwnd,
            on_event: Arc::new(on_event),
            paused: false,
            position_secs: 0.0,
            last_pushed_position: 0.0,
        }
    }

    /// Lazily register the app with the OS media handler and push initial
    /// metadata. Idempotent — safe to call on every `FileLoaded`.
    pub fn ensure_active(&mut self, title: &str, duration_secs: Option<f64>) {
        if self.inner.is_none() {
            // souvlaki 0.8.x on Windows calls .expect() on the HWND and panics
            // when None is passed — bail out rather than abort the process.
            #[cfg(target_os = "windows")]
            if self.hwnd.is_none() {
                return;
            }
            let config = PlatformConfig {
                dbus_name: "putmpv",
                display_name: "PutMPV",
                hwnd: self.hwnd.map(|h| h as *mut c_void),
            };
            let mut controls = match MediaControls::new(config) {
                Ok(c) => c,
                Err(e) => {
                    warn!("media controls unavailable: {e:?}");
                    return;
                }
            };
            let on_event = self.on_event.clone();
            if let Err(e) = controls.attach(move |event| {
                let cmd = match event {
                    MediaControlEvent::Play => Some(MediaCommand::Play),
                    MediaControlEvent::Pause => Some(MediaCommand::Pause),
                    MediaControlEvent::Toggle => Some(MediaCommand::Toggle),
                    MediaControlEvent::Stop => Some(MediaCommand::Stop),
                    MediaControlEvent::Next => Some(MediaCommand::Next),
                    MediaControlEvent::Previous => Some(MediaCommand::Previous),
                    MediaControlEvent::SetPosition(MediaPosition(d)) => {
                        Some(MediaCommand::Seek(d.as_secs_f64()))
                    }
                    _ => None,
                };
                if let Some(cmd) = cmd {
                    on_event(cmd);
                }
            }) {
                warn!("failed to attach media controls: {e:?}");
                return;
            }
            self.inner = Some(controls);
            self.paused = false;
            self.position_secs = 0.0;
            self.last_pushed_position = 0.0;
        }
        self.set_metadata(title, duration_secs);
        self.push_playback();
    }

    /// Update the stored HWND before controls are first activated.
    /// No-op once `ensure_active` has already created the inner controls.
    pub fn update_hwnd(&mut self, hwnd: Option<usize>) {
        if self.inner.is_none() {
            self.hwnd = hwnd;
        }
    }

    /// Drop the OS registration. Releases the D-Bus name on Linux,
    /// unregisters SMTC on Windows, clears Now Playing on macOS.
    pub fn release(&mut self) {
        self.inner = None;
        self.position_secs = 0.0;
        self.last_pushed_position = 0.0;
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.push_playback();
    }

    /// Track the latest known position and push it to the OS if a second
    /// has elapsed since the last push. Cheap when inactive.
    pub fn set_position(&mut self, secs: f64) {
        self.position_secs = secs;
        if self.inner.is_some() && (secs - self.last_pushed_position).abs() >= 1.0 {
            self.push_playback();
        }
    }

    /// Force-flush the current (paused, position) state to the OS.
    /// Use after a seek or when starting a new track.
    pub fn flush_position(&mut self) {
        self.push_playback();
    }

    pub fn set_metadata(&mut self, title: &str, duration_secs: Option<f64>) {
        let Some(controls) = self.inner.as_mut() else {
            return;
        };
        let duration = duration_secs.map(|d| Duration::from_secs_f64(d.max(0.0)));
        if let Err(e) = controls.set_metadata(MediaMetadata {
            title: Some(title),
            duration,
            ..Default::default()
        }) {
            warn!("failed to set media metadata: {e:?}");
        }
    }

    fn push_playback(&mut self) {
        let Some(controls) = self.inner.as_mut() else {
            return;
        };
        let progress = Some(MediaPosition(Duration::from_secs_f64(
            self.position_secs.max(0.0),
        )));
        let state = if self.paused {
            MediaPlayback::Paused { progress }
        } else {
            MediaPlayback::Playing { progress }
        };
        if let Err(e) = controls.set_playback(state) {
            warn!("failed to update media playback state: {e:?}");
        }
        self.last_pushed_position = self.position_secs;
    }
}
