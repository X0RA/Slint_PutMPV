//! OAuth device flow state and startup / sign-in callbacks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use slint::ComponentHandle;
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

use crate::putio::client::ApiError;
use crate::putio::types::UnifiedDirectoryTree;
use crate::putio::{self, oauth, PutioClient};
use crate::storage::config::ConfigStore;
use crate::storage::files_store::FilesStore;
use crate::sync::watch_session::WatchSyncService;
use crate::AppWindow;

use super::state::{OauthFlow, UiState};
use super::{Services, VIEW_CODE, VIEW_FILES, VIEW_LOADING, VIEW_SPLASH};

struct AuthenticatedSessionRefresh {
    weak: slint::Weak<AppWindow>,
    cfg: Arc<ConfigStore>,
    files_store: Arc<FilesStore>,
    client: PutioClient,
    tree: Arc<RwLock<UnifiedDirectoryTree>>,
    sync_profiles: Arc<RwLock<Vec<putio::sync::SyncProfile>>>,
    watch_sync: Arc<WatchSyncService>,
    token: String,
    tree_success_log: Option<&'static str>,
    tree_error_log: &'static str,
}

impl AuthenticatedSessionRefresh {
    async fn run(self) {
        // If a sync profile is configured, block on the initial pull so the
        // user can't kick off playback against stale local state. Cap the
        // wait so an unreachable put.io doesn't lock them out forever — on
        // timeout the next playback simply uses the local store.
        let (slug, _) = self.cfg.file_state_sync_profile();
        if !slug.trim().is_empty() {
            let pull = self.watch_sync.sync_now();
            match tokio::time::timeout(std::time::Duration::from_secs(10), pull).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!("initial sync pull failed: {e}"),
                Err(_) => warn!("initial sync pull timed out after 10s; using local state"),
            }
        }

        let _ = self.weak.upgrade_in_event_loop(|app| {
            app.set_view(VIEW_FILES);
            app.invoke_request_refresh();
        });

        match putio::config_kv::get(&self.client, &self.token, putio::config_kv::TMDB_KEY).await {
            Ok(value) => {
                let _ = self.cfg.set_tmdb_putio_key(&value);
            }
            Err(e) => warn!("refresh put.io TMDB key failed: {e}"),
        }

        match putio::sync::list_profiles(&self.client, &self.token, &self.cfg).await {
            Ok(profiles) => {
                *self.sync_profiles.write().unwrap() = profiles;
            }
            Err(e) => warn!("list sync profiles failed: {e}"),
        }

        let _ = self.weak.upgrade_in_event_loop(|app| {
            app.invoke_settings_refresh();
        });

        match putio::files::build_tree(self.client, self.token).await {
            Ok(new_tree) => {
                if let Some(message) = self.tree_success_log {
                    info!(
                        "{message}: {} folders, {} files",
                        new_tree.total_folders, new_tree.total_files
                    );
                }
                if let Err(e) = self.files_store.write_tree(&new_tree) {
                    error!("write tree: {e}");
                }
                *self.tree.write().unwrap() = new_tree;
                let _ = self.weak.upgrade_in_event_loop(|app| {
                    app.invoke_request_refresh();
                    app.invoke_metadata_criteria_changed();
                    app.invoke_auto_metadata_fetch_after_refresh();
                });
            }
            Err(e) => error!("{}: {e}", self.tree_error_log),
        }
    }
}

pub(crate) fn install(app: &AppWindow, services: &Services, state: &UiState, rt: &Arc<Runtime>) {
    let weak = app.as_weak();
    let client = services.client.clone();
    let cfg = services.config.clone();
    let oauth_flow = state.oauth_flow.clone();
    let tree = state.tree.clone();
    let files_store = services.files_store.clone();
    let sync_profiles = state.sync_profiles.clone();
    let watch_sync = services.watch_sync.clone();

    app.on_sign_in({
        let weak = weak.clone();
        let client = client.clone();
        let cfg = cfg.clone();
        let rt = rt.clone();
        let oauth_flow = oauth_flow.clone();
        let tree = tree.clone();
        let files_store = files_store.clone();
        let sync_profiles = sync_profiles.clone();
        let watch_sync = watch_sync.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_LOADING);
                app.set_loading_message("Requesting code…".into());
                app.set_oauth_error("".into());
            }
            let app_id = cfg.put_client_id();
            let weak_inner = weak.clone();
            let client = client.clone();
            let cfg = cfg.clone();
            let oauth_flow_ref = oauth_flow.clone();
            let tree = tree.clone();
            let files_store = files_store.clone();
            let sync_profiles = sync_profiles.clone();
            let watch_sync = watch_sync.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            oauth_flow_ref.borrow_mut().cancel = Some(cancel.clone());
            rt.spawn(async move {
                let code = match oauth::get_device_code(&client, app_id).await {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = format!("Could not fetch code: {e}");
                        error!("{msg}");
                        let _ = weak_inner.upgrade_in_event_loop(move |app| {
                            app.set_oauth_error(msg.into());
                            app.set_view(VIEW_SPLASH);
                        });
                        return;
                    }
                };
                let display = code.to_uppercase();
                let _ = weak_inner.upgrade_in_event_loop(move |app| {
                    app.set_device_code(display.into());
                    app.set_device_expires("10:00".into());
                    app.set_view(VIEW_CODE);
                });

                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    match oauth::poll_token(&client, &code).await {
                        Ok(oauth::PollResult::Pending) => continue,
                        Ok(oauth::PollResult::Token(token)) => {
                            if let Err(e) = cfg.set_oauth_token(&token) {
                                error!("save token: {e}");
                            }
                            AuthenticatedSessionRefresh {
                                weak: weak_inner.clone(),
                                cfg,
                                files_store,
                                client: client.clone(),
                                tree,
                                sync_profiles,
                                watch_sync,
                                token,
                                tree_success_log: None,
                                tree_error_log: "initial tree build failed",
                            }
                            .run()
                            .await;
                            return;
                        }
                        Err(e) => {
                            let msg = format!("OAuth error: {e}");
                            error!("{msg}");
                            let _ = weak_inner.upgrade_in_event_loop(move |app| {
                                app.set_oauth_error(msg.into());
                                app.set_view(VIEW_SPLASH);
                            });
                            return;
                        }
                    }
                }
            });
        }
    });

    app.on_code_back({
        let weak = weak.clone();
        let oauth_flow = oauth_flow.clone();
        move || {
            if let Some(c) = oauth_flow.borrow().cancel.clone() {
                c.store(true, Ordering::Relaxed);
            }
            *oauth_flow.borrow_mut() = OauthFlow::default();
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_SPLASH);
            }
        }
    });

    app.on_code_open_link({
        let weak = weak.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let code = app.get_device_code().to_string();
            let url = if code.is_empty() {
                "https://app.put.io/link".to_string()
            } else {
                format!("https://app.put.io/link?code={code}")
            };
            if let Err(e) = open::that(&url) {
                warn!("could not open browser: {e}");
            }
        }
    });

    app.on_code_copy({
        let weak = weak.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let code = app.get_device_code().to_string();
            if code.is_empty() {
                return;
            }
            match arboard::Clipboard::new() {
                Ok(mut cb) => {
                    if let Err(e) = cb.set_text(code) {
                        warn!("clipboard write failed: {e}");
                    }
                }
                Err(e) => warn!("clipboard init failed: {e}"),
            }
        }
    });

    app.on_code_continue({
        let weak = weak.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_FILES);
            }
        }
    });

    let path_stack_logout = state.path_stack.clone();
    let current_folder_logout = state.current_folder.clone();
    app.on_logout({
        let weak = weak.clone();
        let cfg = cfg.clone();
        let tree = tree.clone();
        let path_stack = path_stack_logout.clone();
        let current_folder = current_folder_logout.clone();
        move || {
            if let Err(e) = cfg.clear_oauth_token() {
                warn!("clear token: {e}");
            }
            *tree.write().unwrap() = UnifiedDirectoryTree::default();
            *current_folder.borrow_mut() = 0;
            *path_stack.borrow_mut() = vec![(0u64, "put.io".to_string())];
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_SPLASH);
                app.invoke_request_refresh();
            }
        }
    });

    app.on_offline_retry({
        let weak = weak.clone();
        let cfg = cfg.clone();
        let files_store = files_store.clone();
        let client = client.clone();
        let tree = tree.clone();
        let sync_profiles = sync_profiles.clone();
        let watch_sync = watch_sync.clone();
        let rt = rt.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_loading_error(false);
                app.set_loading_message("Checking sign-in…".into());
            }
            spawn_startup_check(
                weak.clone(),
                cfg.clone(),
                files_store.clone(),
                client.clone(),
                tree.clone(),
                sync_profiles.clone(),
                watch_sync.clone(),
                &rt,
            );
        }
    });
}

/// OAuth token check and background tree refresh after UI is wired.
pub(crate) fn run_startup(
    app: &AppWindow,
    services: &Services,
    state: &UiState,
    rt: &Arc<Runtime>,
) {
    spawn_startup_check(
        app.as_weak(),
        services.config.clone(),
        services.files_store.clone(),
        services.client.clone(),
        state.tree.clone(),
        state.sync_profiles.clone(),
        services.watch_sync.clone(),
        rt,
    );
}

/// Verify the stored OAuth token. On success, kick off the post-auth session
/// refresh. On a genuine 401, clear the stored token and route to the splash
/// screen. On a network/server error, *preserve* the token and surface an
/// offline-retry UI on the loading view — re-launching against a flaky
/// connection should not wipe valid credentials.
#[allow(clippy::too_many_arguments)]
fn spawn_startup_check(
    weak: slint::Weak<AppWindow>,
    cfg: Arc<ConfigStore>,
    files_store: Arc<FilesStore>,
    client: PutioClient,
    tree: Arc<RwLock<UnifiedDirectoryTree>>,
    sync_profiles: Arc<RwLock<Vec<putio::sync::SyncProfile>>>,
    watch_sync: Arc<WatchSyncService>,
    rt: &Arc<Runtime>,
) {
    let token = cfg.oauth_token();
    if token.is_empty() {
        let _ = weak.upgrade_in_event_loop(|app| {
            app.set_loading_error(false);
            app.set_view(VIEW_SPLASH);
        });
        return;
    }

    if let Ok(t) = files_store.read_tree() {
        *tree.write().unwrap() = t;
    }

    rt.spawn(async move {
        match oauth::check_token_validity(&client, &token).await {
            Ok(true) => {
                AuthenticatedSessionRefresh {
                    weak,
                    cfg,
                    files_store,
                    client,
                    tree,
                    sync_profiles,
                    watch_sync,
                    token,
                    tree_success_log: Some("tree refresh done"),
                    tree_error_log: "tree refresh failed",
                }
                .run()
                .await;
            }
            Ok(false) => {
                info!("stored token invalid, clearing");
                let _ = cfg.clear_oauth_token();
                let _ = weak.upgrade_in_event_loop(|app| {
                    app.set_loading_error(false);
                    app.set_view(VIEW_SPLASH);
                });
            }
            Err(e) => {
                // Transport, HTTP 5xx, or parse error — none of these prove the
                // token is bad. Keep it, and let the user retry once their
                // connection settles.
                let message = match &e {
                    ApiError::Transport(_) => {
                        "No internet connection. Check your network and retry."
                    }
                    _ => "Could not reach put.io. Check your connection and retry.",
                };
                warn!("startup auth check failed: {e}; preserving token");
                let message = message.to_string();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_loading_error_message(message.into());
                    app.set_loading_error(true);
                    app.set_view(VIEW_LOADING);
                });
            }
        }
    });
}
