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

#[derive(Clone)]
pub struct WatchSyncService {
    store: Arc<RwLock<FileStateStore>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    session: Arc<Mutex<Option<WatchSession>>>,
    sync_lock: Arc<tokio::sync::Mutex<()>>,
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
        });
        service.start_scheduler();
        service
    }

    pub fn pull_to_local(&self) {
        self.flush_background(false);
    }

    pub fn start_session(&self, file_id: u64, duration_hint: f64) -> Option<f64> {
        self.pull_to_local_blocking();
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
                *self.store.write().unwrap() = store;
            }
            Err(e) => warn!("could not flush watched state on shutdown: {e}"),
        }
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

    fn pull_to_local_blocking(&self) {
        let token = self.config.oauth_token();
        let (slug, _) = self.config.file_state_sync_profile();
        if token.is_empty() || slug.trim().is_empty() {
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
                *self.store.write().unwrap() = store;
            }
            Err(e) => warn!("could not pull watched state before playback: {e}"),
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
        let shared_store = self.store.clone();
        let client = self.client.clone();
        let service = self.clone();
        let lock = self.sync_lock.clone();
        self.rt.spawn(async move {
            let _guard = lock.lock().await;
            match putio::sync::sync_profile(&client, &token, &mut store, &slug).await {
                Ok(()) => {
                    *shared_store.write().unwrap() = store;
                    if update_session_snapshot {
                        service.mark_session_flushed();
                    }
                }
                Err(e) => warn!("could not sync watched state: {e}"),
            }
        });
    }

    fn mark_session_flushed(&self) {
        let mut session = self.session.lock().unwrap();
        if let Some(session) = session.as_mut() {
            session.last_pushed_position = session.current_position;
            session.last_pushed_at = Instant::now();
            session.dirty = false;
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
