use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tracing::warn;

use crate::putio::{self, PutioClient};
use crate::storage::config::ConfigStore;
use crate::storage::file_state::FileStateStore;

const PAUSE_DEBOUNCE: Duration = Duration::from_secs(4);
const SEEK_DEBOUNCE: Duration = Duration::from_secs(2);
const HEARTBEAT: Duration = Duration::from_secs(5 * 60);
const DIRTY_POSITION_DELTA: f64 = 0.5;

type RefreshNotifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct WatchSyncService {
    store: Arc<RwLock<FileStateStore>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    session: Arc<Mutex<Option<WatchSession>>>,
    sync_lock: Arc<tokio::sync::Mutex<()>>,
    refresh_notifier: Arc<RwLock<Option<RefreshNotifier>>>,
    // Outstanding flush_background task handles. shutdown_blocking drains
    // these with a short timeout so an in-flight upload+trash cycle gets to
    // finish before the runtime is dropped.
    pending_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug)]
struct WatchSession {
    file_id: u64,
    duration: f64,
    current_position: f64,
    last_pushed_position: f64,
    last_pushed_at: Instant,
    pending_pause_at: Option<Instant>,
    pending_seek_at: Option<Instant>,
    paused: bool,
    finished: bool,
    dirty: bool,
}

impl WatchSyncService {
    pub fn new(
        store: Arc<RwLock<FileStateStore>>,
        client: PutioClient,
        config: Arc<ConfigStore>,
        rt: Arc<Runtime>,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            store,
            client,
            config,
            rt,
            session: Arc::new(Mutex::new(None)),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            refresh_notifier: Arc::new(RwLock::new(None)),
            pending_tasks: Arc::new(Mutex::new(Vec::new())),
        });
        service.start_scheduler();
        service
    }

    pub fn set_refresh_notifier<F>(&self, notifier: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.refresh_notifier.write().unwrap() = Some(Arc::new(notifier));
    }

    fn notify_refresh(&self) {
        let notifier = self.refresh_notifier.read().unwrap().clone();
        if let Some(notifier) = notifier {
            notifier();
        }
    }

    /// Run the full sync_profile dance against the currently-configured profile,
    /// holding sync_lock and merging the result back into the live store so
    /// concurrent writes (playback positions, mark-watched) survive.
    pub async fn sync_now(&self) -> anyhow::Result<()> {
        let token = self.config.oauth_token();
        if token.is_empty() {
            return Err(anyhow::anyhow!("sign in before syncing watched state"));
        }
        let (slug, _) = self.config.file_state_sync_profile();
        if slug.trim().is_empty() {
            return Err(anyhow::anyhow!("no sync profile selected"));
        }
        let mut store = self.store.read().unwrap().clone();
        let result = {
            let _guard = self.sync_lock.lock().await;
            putio::sync::sync_profile(&self.client, &token, &mut store, &slug).await
        };
        result?;
        self.merge_remote_into_live(store.entries());
        Ok(())
    }

    /// Switch to (or create) the named profile and sync against it. Holds
    /// sync_lock and merges the result back into the live store.
    pub async fn use_profile(&self, name: &str) -> anyhow::Result<String> {
        let token = self.config.oauth_token();
        if token.is_empty() {
            return Err(anyhow::anyhow!("sign in before using shared profiles"));
        }
        let mut store = self.store.read().unwrap().clone();
        let slug = {
            let _guard = self.sync_lock.lock().await;
            putio::sync::select_profile(&self.client, &token, &self.config, &mut store, name)
                .await?
        };
        self.merge_remote_into_live(store.entries());
        Ok(slug)
    }

    pub fn start_session(&self, file_id: u64, duration_hint: f64) -> Option<f64> {
        self.flush_background(false);
        let duration = finite_nonnegative(duration_hint);
        let saved = self
            .store
            .read()
            .unwrap()
            .entries()
            .get(&file_id.to_string())
            .copied();
        let position = saved.map(|entry| entry.position_secs).unwrap_or(0.0);
        let saved_duration = saved.map(|entry| entry.duration_secs).unwrap_or(0.0);
        let resume_duration = if duration > 0.0 {
            duration
        } else {
            saved_duration
        };
        *self.session.lock().unwrap() = Some(WatchSession {
            file_id,
            duration: resume_duration,
            current_position: position,
            last_pushed_position: position,
            last_pushed_at: Instant::now(),
            pending_pause_at: None,
            pending_seek_at: None,
            paused: false,
            finished: false,
            dirty: false,
        });
        if resume_duration > 0.0 && position >= 30.0 && position < resume_duration - 30.0 {
            Some(position)
        } else {
            None
        }
    }

    pub fn on_position(&self, position: f64, duration: f64) {
        let Some((file_id, duration)) = self.update_session_position(position, duration) else {
            return;
        };
        self.update_local_position(file_id, position, duration);
    }

    pub fn on_duration(&self, duration: f64) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            let duration = finite_nonnegative(duration);
            if duration > 0.0 {
                session.duration = duration;
            }
        }
    }

    pub fn on_pause(&self, paused: bool) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.paused = paused;
            if paused {
                session.pending_pause_at = Some(Instant::now() + PAUSE_DEBOUNCE);
            } else {
                session.pending_pause_at = None;
            }
        }
    }

    pub fn on_seek(&self) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.pending_seek_at = Some(Instant::now() + SEEK_DEBOUNCE);
            session.dirty = true;
        }
    }

    pub fn on_eof(&self) {
        let file_id = {
            let mut session = self.session.lock().unwrap();
            let Some(session) = session.as_mut() else {
                return;
            };
            session.finished = true;
            session.dirty = true;
            session.file_id
        };
        {
            let mut store = self.store.write().unwrap();
            store.set_watched(file_id, true);
            if let Err(e) = store.save() {
                warn!("could not save EOF watched state: {e}");
            }
        }
        self.flush_background(true);
    }

    pub fn on_session_end(&self) {
        let dirty = self
            .session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.dirty);
        if dirty {
            self.flush_background(true);
        }
        *self.session.lock().unwrap() = None;
    }

    pub fn shutdown_blocking(&self) {
        let dirty = self
            .session
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.dirty);
        *self.session.lock().unwrap() = None;

        // First, give any in-flight flush_background tasks a chance to land
        // their upload + trash cycle. Without this they get cancelled when
        // the runtime drops in main(), leaving duplicates on put.io.
        self.drain_pending_tasks(std::time::Duration::from_secs(5));

        if !dirty {
            if let Err(e) = self.store.read().unwrap().save() {
                warn!("could not save local watched state on shutdown: {e}");
            }
            return;
        }
        let token = self.config.oauth_token();
        let (slug, _) = self.config.file_state_sync_profile();
        if token.is_empty() || slug.trim().is_empty() {
            if let Err(e) = self.store.read().unwrap().save() {
                warn!("could not save local watched state on shutdown: {e}");
            }
            return;
        }
        let mut store = self.store.read().unwrap().clone();
        let lock = self.sync_lock.clone();
        let result = self.rt.block_on(async move {
            let _guard = lock.lock().await;
            putio::sync::sync_profile(&self.client, &token, &mut store, &slug)
                .await
                .map(|_| store)
        });
        match result {
            Ok(store) => {
                self.merge_remote_into_live(store.entries());
            }
            Err(e) => warn!("could not flush watched state on shutdown: {e}"),
        }
    }

    fn drain_pending_tasks(&self, timeout: std::time::Duration) {
        let handles: Vec<_> = std::mem::take(&mut *self.pending_tasks.lock().unwrap());
        if handles.is_empty() {
            return;
        }
        self.rt.block_on(async move {
            for handle in handles {
                if handle.is_finished() {
                    let _ = handle.await;
                    continue;
                }
                match tokio::time::timeout(timeout, handle).await {
                    Ok(_) => {}
                    Err(_) => warn!("flush task still running at shutdown; cancelling"),
                }
            }
        });
    }

    pub fn mark_watched(&self, file_id: u64, watched: bool) {
        {
            let mut store = self.store.write().unwrap();
            store.set_watched(file_id, watched);
            if let Err(e) = store.save() {
                warn!("could not save watched state: {e}");
            }
        }
        {
            let mut session = self.session.lock().unwrap();
            if let Some(session) = session.as_mut().filter(|s| s.file_id == file_id) {
                session.dirty = true;
                session.last_pushed_position = if watched {
                    session.current_position
                } else {
                    0.0
                };
            }
        }
        self.flush_background(true);
    }

    fn start_scheduler(self: &Arc<Self>) {
        let service = self.clone();
        self.rt.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                service.tick();
            }
        });
    }

    fn merge_remote_into_live(&self, remote: &std::collections::BTreeMap<String, crate::storage::file_state::FileStateEntry>) {
        let changed = {
            let mut live = self.store.write().unwrap();
            let changed = live.merge(remote);
            if changed {
                if let Err(e) = live.save() {
                    warn!("could not save merged watch state: {e}");
                }
            }
            changed
        };
        if changed {
            self.notify_refresh();
        }
    }

    fn tick(&self) {
        let should_flush = {
            let now = Instant::now();
            let mut session = self.session.lock().unwrap();
            let Some(session) = session.as_mut() else {
                return;
            };
            let pause_due = session.pending_pause_at.is_some_and(|at| at <= now);
            let seek_due = session.pending_seek_at.is_some_and(|at| at <= now);
            let heartbeat_due = !session.paused
                && session.dirty
                && now.duration_since(session.last_pushed_at) >= HEARTBEAT;
            if pause_due {
                session.pending_pause_at = None;
            }
            if seek_due {
                session.pending_seek_at = None;
            }
            pause_due || seek_due || heartbeat_due
        };
        if should_flush {
            self.flush_background(true);
        }
    }

    fn update_session_position(&self, position: f64, duration: f64) -> Option<(u64, f64)> {
        let mut session = self.session.lock().unwrap();
        let session = session.as_mut()?;
        let position = finite_nonnegative(position);
        let duration = finite_nonnegative(duration);
        if duration > 0.0 {
            session.duration = duration;
        }
        session.current_position = position;
        if (session.current_position - session.last_pushed_position).abs() > DIRTY_POSITION_DELTA {
            session.dirty = true;
        }
        Some((session.file_id, session.duration))
    }

    fn update_local_position(&self, file_id: u64, position: f64, duration: f64) {
        let mut store = self.store.write().unwrap();
        store.update_position(file_id, position, duration);
        if let Err(e) = store.save() {
            warn!("could not save playback position: {e}");
        }
    }

    fn flush_background(&self, update_session_snapshot: bool) {
        let token = self.config.oauth_token();
        let (slug, _) = self.config.file_state_sync_profile();
        if token.is_empty() || slug.trim().is_empty() {
            if let Err(e) = self.store.read().unwrap().save() {
                warn!("could not save local watched state: {e}");
            }
            if update_session_snapshot {
                self.mark_session_flushed();
            }
            return;
        }

        let mut store = self.store.read().unwrap().clone();
        let client = self.client.clone();
        let service = self.clone();
        let lock = self.sync_lock.clone();
        let handle = self.rt.spawn(async move {
            let _guard = lock.lock().await;
            match putio::sync::sync_profile(&client, &token, &mut store, &slug).await {
                Ok(()) => {
                    service.merge_remote_into_live(store.entries());
                    if update_session_snapshot {
                        service.mark_session_flushed();
                    }
                }
                Err(e) => {
                    warn!("could not sync watched state: {e}");
                    // Treat the failure as a "push attempt" so the heartbeat
                    // (gated on HEARTBEAT since last_pushed_at) doesn't fire
                    // another flush on every tick. dirty stays true so the
                    // next heartbeat or user action will retry, just not
                    // every second.
                    if update_session_snapshot {
                        service.mark_flush_attempted();
                    }
                }
            }
        });
        let mut tasks = self.pending_tasks.lock().unwrap();
        tasks.retain(|h| !h.is_finished());
        tasks.push(handle);
    }

    fn mark_session_flushed(&self) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.last_pushed_position = session.current_position;
            session.last_pushed_at = Instant::now();
            session.dirty = false;
        }
    }

    fn mark_flush_attempted(&self) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.last_pushed_at = Instant::now();
        }
    }
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
