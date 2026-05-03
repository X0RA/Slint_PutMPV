//! Settings page: TMDB keys, sync profiles, local data rows/clear.

use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};
use tokio::runtime::Runtime;
use tracing::warn;

use crate::putio::types::UnifiedDirectoryTree;
use crate::putio::{self};
use crate::storage::file_state::{count_played, FileStateStore};
use crate::{AppWindow, LocalDataRow};

use super::state::UiState;
use super::util::{path_label, source_to_index};
use super::{Services, VIEW_SPLASH_AFTER_RESET};

pub(crate) fn install(
    app: &AppWindow,
    state: &UiState,
    services: &Services,
    request_refresh: Rc<dyn Fn()>,
    rt: &Arc<Runtime>,
) {
    let weak = app.as_weak();
    let config = services.config.clone();
    let files_store = services.files_store.clone();
    let matched_store = services.matched_store.clone();
    let tmdb_store = services.tmdb_store.clone();
    let tvmaze_store = services.tvmaze_store.clone();
    let file_state = services.file_state.clone();
    let sync_profiles = state.sync_profiles.clone();
    let pending_local_clear = state.pending_local_clear.clone();
    let tree = state.tree.clone();
    let current_folder = state.current_folder.clone();
    let path_stack = state.path_stack.clone();

    app.on_settings_refresh({
        let weak = weak.clone();
        let config = config.clone();
        let files_store = files_store.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let tvmaze_store = tvmaze_store.clone();
        let file_state = file_state.clone();
        let sync_profiles = sync_profiles.clone();
        move || {
            let Some(app) = weak.upgrade() else { return; };
            let local_key = config.tmdb_local_key();
            let putio_key = config.tmdb_putio_key();
            let tmdb_source = source_to_index(&config.tmdb_source());

            app.set_tmdb_local_key(local_key.into());
            app.set_tmdb_putio_key(putio_key.into());
            app.set_tmdb_source(tmdb_source);
            app.set_auto_metadata_fetch_enabled(config.auto_metadata_fetch());

            let profiles = sync_profiles.read().unwrap();
            let labels = profiles
                .iter()
                .map(|p| {
                    if p.total_played > 0 {
                        format!("{} ({})", p.name, p.total_played).into()
                    } else {
                        p.name.as_str().into()
                    }
                })
                .collect::<Vec<slint::SharedString>>();
            app.set_sync_profile_labels(ModelRc::from(Rc::new(VecModel::from(labels))));
            let (active_slug, active_name) = config.file_state_sync_profile();
            app.set_sync_active_profile(active_name.into());
            if !active_slug.is_empty() {
                if let Some(index) = profiles.iter().position(|p| p.slug == active_slug) {
                    app.set_sync_existing_index(index as i32);
                }
            }
            drop(profiles);

            let state_entries = file_state.read().unwrap();
            app.set_sync_known_count(state_entries.entries().len() as i32);
            app.set_sync_played_count(count_played(state_entries.entries()) as i32);
            drop(state_entries);

            let config_path = config.path();
            let file_state_path = file_state.read().unwrap().path();
            let files_path = files_store.path();
            let matched_path = matched_store.path();
            let tmdb_path = tmdb_store.path();
            let tvmaze_path = tvmaze_store.path();
            let rows = vec![
                LocalDataRow {
                    name: "App configuration".into(),
                    desc: "OAuth token, TMDB keys and selected sync profile.".into(),
                    path: path_label(&config_path).into(),
                    enabled: true,
                },
                LocalDataRow {
                    name: "Played file state".into(),
                    desc: "Local file store for played markers. Clearing resets played flags while keeping entries.".into(),
                    path: path_label(&file_state_path).into(),
                    enabled: true,
                },
                LocalDataRow {
                    name: "Files data".into(),
                    desc: "Cached directory tree and file information.".into(),
                    path: path_label(&files_path).into(),
                    enabled: true,
                },
                LocalDataRow {
                    name: "Matched data".into(),
                    desc: "File to movie and TV episode ID mappings.".into(),
                    path: path_label(&matched_path).into(),
                    enabled: true,
                },
                LocalDataRow {
                    name: "TMDB data".into(),
                    desc: "Cached movie and TV show metadata from TMDB.".into(),
                    path: path_label(&tmdb_path).into(),
                    enabled: true,
                },
                LocalDataRow {
                    name: "TVMaze data".into(),
                    desc: "Cached TV show metadata from TVMaze.".into(),
                    path: path_label(&tvmaze_path).into(),
                    enabled: true,
                },
            ];
            app.set_local_data_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
    });

    app.invoke_settings_refresh();

    app.on_tmdb_source_changed({
        let weak = weak.clone();
        let cfg = config.clone();
        move |source| {
            let value = if source == 1 { "putio" } else { "local" };
            if let Err(e) = cfg.set_tmdb_source(value) {
                warn!("save TMDB source: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.set_tmdb_status(
                    format!(
                        "Using {} TMDB key.",
                        if source == 1 {
                            "put.io shared"
                        } else {
                            "local"
                        }
                    )
                    .into(),
                );
                app.invoke_settings_refresh();
            }
        }
    });

    app.on_tmdb_save_local({
        let weak = weak.clone();
        let cfg = config.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let key = app.get_tmdb_local_key().to_string();
            match cfg.set_tmdb_local_key(&key) {
                Ok(()) => app.set_tmdb_status("Saved local TMDB key.".into()),
                Err(e) => app.set_tmdb_status(format!("Could not save local key: {e}").into()),
            }
            app.invoke_settings_refresh();
        }
    });

    app.on_tmdb_replace_putio({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let value = app.get_tmdb_local_key().to_string();
            let token = cfg.oauth_token();
            if token.is_empty() {
                app.set_tmdb_status("Sign in before updating the put.io shared key.".into());
                return;
            }
            app.set_tmdb_status("Updating shared TMDB key on put.io...".into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message = match putio::config_kv::put(
                    &client,
                    &token,
                    putio::config_kv::TMDB_KEY,
                    &value,
                )
                .await
                {
                    Ok(()) => {
                        match putio::config_kv::get(&client, &token, putio::config_kv::TMDB_KEY)
                            .await
                        {
                            Ok(fresh) => {
                                let _ = cfg.set_tmdb_putio_key(&fresh);
                                "Updated shared TMDB key on put.io.".to_string()
                            }
                            Err(e) => format!("Updated shared key, but refresh failed: {e}"),
                        }
                    }
                    Err(e) => format!("Could not update shared key: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_tmdb_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_tmdb_delete_putio({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        move || {
            let token = cfg.oauth_token();
            let Some(app) = weak.upgrade() else {
                return;
            };
            if token.is_empty() {
                app.set_tmdb_status("Sign in before deleting the put.io shared key.".into());
                return;
            }
            app.set_tmdb_status("Deleting shared TMDB key from put.io...".into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message =
                    match putio::config_kv::delete(&client, &token, putio::config_kv::TMDB_KEY)
                        .await
                    {
                        Ok(()) => {
                            let _ = cfg.set_tmdb_putio_key("");
                            "Deleted shared TMDB key from put.io.".to_string()
                        }
                        Err(e) => format!("Could not delete shared key: {e}"),
                    };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_tmdb_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_auto_metadata_fetch_changed({
        let weak = weak.clone();
        let cfg = config.clone();
        move |enabled| {
            if let Some(app) = weak.upgrade() {
                app.set_auto_metadata_fetch_enabled(enabled);
            }
            if let Err(e) = cfg.set_auto_metadata_fetch(enabled) {
                warn!("save automatic metadata preference: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.invoke_settings_refresh();
            }
        }
    });

    app.on_sync_profile_selected({
        let weak = weak.clone();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_sync_existing_index(index);
            }
        }
    });

    app.on_sync_refresh_profiles({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        let sync_profiles = sync_profiles.clone();
        move || {
            let token = cfg.oauth_token();
            let Some(app) = weak.upgrade() else {
                return;
            };
            if token.is_empty() {
                app.set_sync_status("Sign in before listing shared profiles.".into());
                return;
            }
            app.set_sync_status("Loading shared profiles from put.io...".into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            let sync_profiles = sync_profiles.clone();
            rt.spawn(async move {
                let message = match putio::sync::list_profiles(&client, &token, &cfg).await {
                    Ok(profiles) => {
                        let count = profiles.len();
                        *sync_profiles.write().unwrap() = profiles;
                        if count == 0 {
                            "No shared profiles found in the PutMPV folder on put.io.".to_string()
                        } else {
                            format!(
                                "Loaded {count} shared profile{} from put.io.",
                                if count == 1 { "" } else { "s" }
                            )
                        }
                    }
                    Err(e) => format!("Could not load shared profiles from put.io: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_sync_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_sync_use_existing({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        let profiles = sync_profiles.clone();
        let file_state = file_state.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let index = app.get_sync_existing_index();
            let Some(profile) = profiles.read().unwrap().get(index as usize).cloned() else {
                app.set_sync_status("Choose a shared profile first.".into());
                return;
            };
            let token = cfg.oauth_token();
            if token.is_empty() {
                app.set_sync_status("Sign in before using shared profiles.".into());
                return;
            }
            app.set_sync_status(format!("Using shared profile {}...", profile.name).into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            let file_state = file_state.clone();
            rt.spawn(async move {
                let message = match FileStateStore::load() {
                    Ok(mut store) => match putio::sync::select_profile(
                        &client,
                        &token,
                        &cfg,
                        &mut store,
                        &profile.name,
                    )
                    .await
                    {
                        Ok(_) => {
                            *file_state.write().unwrap() = store;
                            format!("Using shared profile {}.", profile.name)
                        }
                        Err(e) => format!("Could not use profile: {e}"),
                    },
                    Err(e) => format!("Could not load local file state: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_sync_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_sync_use_new({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        let file_state = file_state.clone();
        let sync_profiles = sync_profiles.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let name = app.get_sync_new_name().to_string();
            let token = cfg.oauth_token();
            if token.is_empty() {
                app.set_sync_status("Sign in before creating shared profiles.".into());
                return;
            }
            app.set_sync_status(format!("Creating or using profile {}...", name).into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            let file_state = file_state.clone();
            let sync_profiles = sync_profiles.clone();
            rt.spawn(async move {
                let message = match FileStateStore::load() {
                    Ok(mut store) => {
                        match putio::sync::select_profile(&client, &token, &cfg, &mut store, &name)
                            .await
                        {
                            Ok(_) => {
                                *file_state.write().unwrap() = store;
                                if let Ok(profiles) =
                                    putio::sync::list_profiles(&client, &token, &cfg).await
                                {
                                    *sync_profiles.write().unwrap() = profiles;
                                }
                                format!("Using shared profile {}.", name)
                            }
                            Err(e) => format!("Could not create/use profile: {e}"),
                        }
                    }
                    Err(e) => format!("Could not load local file state: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_sync_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_sync_now({
        let weak = weak.clone();
        let cfg = config.clone();
        let client = services.client.clone();
        let rt = rt.clone();
        let file_state = file_state.clone();
        move || {
            let token = cfg.oauth_token();
            let Some(app) = weak.upgrade() else {
                return;
            };
            if token.is_empty() {
                app.set_sync_status("Sign in before syncing watched state.".into());
                return;
            }
            app.set_sync_status("Syncing watched state...".into());
            let weak = weak.clone();
            let cfg = cfg.clone();
            let client = client.clone();
            let file_state = file_state.clone();
            rt.spawn(async move {
                let message = match FileStateStore::load() {
                    Ok(mut store) => {
                        match putio::sync::sync_now(&client, &token, &cfg, &mut store).await {
                            Ok(()) => {
                                *file_state.write().unwrap() = store;
                                "Watched state synced.".to_string()
                            }
                            Err(e) => format!("Could not sync watched state: {e}"),
                        }
                    }
                    Err(e) => format!("Could not load local file state: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_sync_status(message.into());
                    app.invoke_settings_refresh();
                });
            });
        }
    });

    app.on_sync_disable({
        let weak = weak.clone();
        let cfg = config.clone();
        move || {
            let message = match putio::sync::disable_sync(&cfg) {
                Ok(()) => "Watch sync disabled.".to_string(),
                Err(e) => format!("Could not disable sync: {e}"),
            };
            if let Some(app) = weak.upgrade() {
                app.set_sync_status(message.into());
                app.invoke_settings_refresh();
            }
        }
    });

    app.on_local_data_clear({
        let weak = weak.clone();
        let pending = pending_local_clear.clone();
        let file_state = file_state.clone();
        let files_store = files_store.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let tvmaze_store = tvmaze_store.clone();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let request_refresh = request_refresh.clone();
        move |index| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            match index {
                0 => {
                    *pending.borrow_mut() = Some(index);
                    app.set_confirm_open(true);
                }
                1 => {
                    let message = {
                        let mut state = file_state.write().unwrap();
                        let changed = state.clear_played();
                        match state.save() {
                            Ok(()) if changed => "Cleared played flags.".to_string(),
                            Ok(()) => "No played flags to clear.".to_string(),
                            Err(e) => format!("Could not clear played flags: {e}"),
                        }
                    };
                    app.set_local_data_status(message.into());
                    app.invoke_settings_refresh();
                }
                2 => {
                    let message = match files_store.clear() {
                        Ok(()) => {
                            *tree.write().unwrap() = UnifiedDirectoryTree::default();
                            *current_folder.borrow_mut() = 0;
                            *path_stack.borrow_mut() = vec![(0u64, "put.io".to_string())];
                            request_refresh();
                            "Cleared cached files data.".to_string()
                        }
                        Err(e) => format!("Could not clear files data: {e}"),
                    };
                    app.set_local_data_status(message.into());
                    app.invoke_settings_refresh();
                }
                3 => {
                    let message = match matched_store.clear() {
                        Ok(()) => "Cleared matched metadata data.".to_string(),
                        Err(e) => format!("Could not clear matched data: {e}"),
                    };
                    app.set_local_data_status(message.into());
                    app.invoke_settings_refresh();
                    app.invoke_metadata_criteria_changed();
                }
                4 => {
                    let message = match tmdb_store.clear_cache() {
                        Ok(()) => "Cleared TMDB metadata cache.".to_string(),
                        Err(e) => format!("Could not clear TMDB cache: {e}"),
                    };
                    app.set_local_data_status(message.into());
                    app.invoke_settings_refresh();
                }
                5 => {
                    let message = match tvmaze_store.clear_cache() {
                        Ok(()) => "Cleared TVMaze metadata cache.".to_string(),
                        Err(e) => format!("Could not clear TVMaze cache: {e}"),
                    };
                    app.set_local_data_status(message.into());
                    app.invoke_settings_refresh();
                }
                _ => {
                    app.set_local_data_status(
                        "That local data store is not implemented in the Rust app yet.".into(),
                    );
                }
            }
        }
    });

    app.on_local_data_cancel_clear({
        let weak = weak.clone();
        let pending = pending_local_clear.clone();
        move || {
            *pending.borrow_mut() = None;
            if let Some(app) = weak.upgrade() {
                app.set_confirm_open(false);
            }
        }
    });

    app.on_local_data_confirm_clear({
        let weak = weak.clone();
        let pending = pending_local_clear.clone();
        let cfg = config.clone();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        move || {
            let Some(index) = *pending.borrow() else {
                return;
            };
            if index != 0 {
                return;
            }
            let message = match cfg.reset_to_defaults() {
                Ok(()) => "App configuration reset.".to_string(),
                Err(e) => format!("Could not reset app configuration: {e}"),
            };
            *pending.borrow_mut() = None;
            *tree.write().unwrap() = UnifiedDirectoryTree::default();
            *current_folder.borrow_mut() = 0;
            *path_stack.borrow_mut() = vec![(0u64, "put.io".to_string())];
            if let Some(app) = weak.upgrade() {
                app.set_confirm_open(false);
                app.set_local_data_status(message.into());
                app.set_view(VIEW_SPLASH_AFTER_RESET);
                app.invoke_request_refresh();
                app.invoke_settings_refresh();
            }
        }
    });
}
