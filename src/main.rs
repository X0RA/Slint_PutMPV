use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use slint::{ModelRc, VecModel};
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

mod mpv;
mod putio;
mod storage;

use putio::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};
use putio::{oauth, PutioClient};
use storage::config::ConfigStore;
use storage::file_state::{count_played, FileStateStore};
use storage::files_store::FilesStore;

slint::include_modules!();

const VIEW_LOADING: i32 = 0;
const VIEW_SPLASH: i32 = 1;
const VIEW_CODE: i32 = 2;
const VIEW_FILES: i32 = 3;
const VIEW_SPLASH_AFTER_RESET: i32 = VIEW_SPLASH;

#[derive(Default)]
struct OauthFlow {
    cancel: Option<Arc<AtomicBool>>,
}

fn empty_file_item() -> FileItem {
    FileItem {
        id: -1,
        item_type: "".into(),
        name: "".into(),
        kind: "".into(),
        grid_meta: "".into(),
        list_kind: "".into(),
        list_size: "".into(),
        list_updated: "".into(),
        detail_size: "".into(),
        detail_kind: "".into(),
        detail_extra_a_label: "".into(),
        detail_extra_a_value: "".into(),
        detail_extra_b_label: "".into(),
        detail_extra_b_value: "".into(),
        location: "".into(),
        is_media: false,
    }
}

fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn format_updated(ts: &Option<String>) -> String {
    let Some(s) = ts else {
        return String::new();
    };
    if s.len() < 10 {
        return s.clone();
    }
    let date = &s[..10];
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return date.to_string();
    }
    let year = parts[0];
    let month: usize = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = months
        .get(month.saturating_sub(1))
        .copied()
        .unwrap_or("???");
    format!("{m} {day:02}, {year}")
}

fn kind_for(file: &PutIoFile) -> (&'static str, &'static str) {
    match file.file_type.as_str() {
        "FOLDER" => ("folder", "FOLDER"),
        "VIDEO" => ("movie", "MOVIE"),
        "AUDIO" => ("music", "MUSIC"),
        "IMAGE" => ("image", "IMAGE"),
        "TEXT" | "PDF" | "ARCHIVE" => ("document", "DOCUMENT"),
        _ => ("file", "FILE"),
    }
}

fn truncate_id(id: u64) -> i32 {
    if id > i32::MAX as u64 {
        warn!("put.io id {id} exceeds i32::MAX; truncating");
    }
    (id & 0x7FFF_FFFF) as i32
}

fn count_in_node(node: &DirectoryNode, total_size: &mut u64, files: &mut u64, folders: &mut u64) {
    *folders += node.children.len() as u64;
    for f in &node.files {
        *files += 1;
        *total_size += f.size;
    }
    for c in &node.children {
        count_in_node(c, total_size, files, folders);
    }
}

fn find_node_by_id(node: &DirectoryNode, id: u64) -> Option<&DirectoryNode> {
    if id == 0 {
        return Some(node);
    }
    for c in &node.children {
        if let Some(f) = &c.file {
            if f.id == id {
                return Some(c);
            }
        }
        if let Some(found) = find_node_by_id(c, id) {
            return Some(found);
        }
    }
    None
}

#[derive(Clone)]
struct DisplayEntry {
    file: PutIoFile,
    aggregate_size: u64,
    folder_item_count: u64,
}

fn node_total_size(node: &DirectoryNode) -> u64 {
    let mut total = 0u64;
    for f in &node.files {
        total += f.size;
    }
    for c in &node.children {
        total += node_total_size(c);
    }
    total
}

fn node_item_count(node: &DirectoryNode) -> u64 {
    (node.children.len() + node.files.len()) as u64
}

fn children_for_folder(tree: &UnifiedDirectoryTree, folder_id: u64) -> Vec<DisplayEntry> {
    let Some(node) = find_node_by_id(&tree.root, folder_id) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(node.children.len() + node.files.len());
    for c in &node.children {
        if let Some(f) = &c.file {
            out.push(DisplayEntry {
                file: f.clone(),
                aggregate_size: node_total_size(c),
                folder_item_count: node_item_count(c),
            });
        }
    }
    for f in &node.files {
        out.push(DisplayEntry {
            file: f.clone(),
            aggregate_size: f.size,
            folder_item_count: 0,
        });
    }
    out
}

fn put_to_file_item(entry: &DisplayEntry, location: &str) -> FileItem {
    let file = &entry.file;
    let is_folder = file.file_type == "FOLDER";
    let (kind, detail_kind) = kind_for(file);
    let updated = format_updated(&file.updated_at);
    let (list_size, detail_size) = if is_folder {
        let s = format_size(entry.aggregate_size);
        (s.clone(), s)
    } else {
        let s = format_size(file.size);
        (s.clone(), s)
    };
    let grid_meta = if is_folder {
        if updated.is_empty() {
            format!("{} items · {}", entry.folder_item_count, list_size)
        } else {
            format!(
                "{} items · {} · {}",
                entry.folder_item_count, list_size, updated
            )
        }
    } else if updated.is_empty() {
        list_size.clone()
    } else {
        format!("{} · {}", list_size, updated)
    };
    let list_kind = if is_folder {
        "Folder".to_string()
    } else if kind == "movie" || kind == "tv" {
        "Media".to_string()
    } else {
        kind.to_string()
    };
    let item_type = if is_folder { "folder" } else { "file" };
    let is_media = matches!(kind, "movie" | "tv" | "music");
    FileItem {
        id: truncate_id(file.id),
        item_type: item_type.into(),
        name: file.name.as_str().into(),
        kind: kind.into(),
        grid_meta: grid_meta.into(),
        list_kind: list_kind.into(),
        list_size: list_size.into(),
        list_updated: updated.as_str().into(),
        detail_size: detail_size.into(),
        detail_kind: detail_kind.into(),
        detail_extra_a_label: "".into(),
        detail_extra_a_value: "".into(),
        detail_extra_b_label: "".into(),
        detail_extra_b_value: "".into(),
        location: location.into(),
        is_media,
    }
}

fn location_text(stack: &[(u64, String)]) -> String {
    stack
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn source_to_index(source: &str) -> i32 {
    match source {
        "putio" | "custom" => 1,
        "managed" => 2,
        _ => 0,
    }
}

fn mpv_source_from_index(index: i32) -> &'static str {
    match index {
        1 => "custom",
        2 => "managed",
        _ => "system",
    }
}

fn path_label(path: &std::path::Path) -> String {
    path.to_string_lossy().to_string()
}

fn install_hint() -> (String, bool, &'static str) {
    #[cfg(target_os = "linux")]
    {
        (
            "MPV not found. Install it with your distro package manager, for example pacman -S mpv or apt install mpv.".to_string(),
            false,
            "",
        )
    }
    #[cfg(target_os = "macos")]
    {
        (
            "IINA not found. Download and install IINA from iina.io.".to_string(),
            true,
            "https://iina.io/",
        )
    }
    #[cfg(target_os = "windows")]
    {
        (
            "mpv.net not found. Download mpv.net from the latest releases page.".to_string(),
            true,
            "https://github.com/mpvnet-player/mpv.net/releases/latest",
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        (
            "MPV not found. Install MPV and configure a custom binary path.".to_string(),
            false,
            "",
        )
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,putmpv=debug".into()),
        )
        .init();

    let config = Arc::new(ConfigStore::load()?);
    let files_store = Arc::new(FilesStore::load()?);
    let file_state = Arc::new(RwLock::new(FileStateStore::load()?));
    let client = PutioClient::new();
    let rt = Arc::new(Runtime::new()?);

    let app = AppWindow::new()?;
    app.set_view(VIEW_LOADING);
    app.set_loading_message("Checking sign-in…".into());

    // Tree is shared across threads
    let tree: Arc<RwLock<UnifiedDirectoryTree>> =
        Arc::new(RwLock::new(UnifiedDirectoryTree::default()));
    let sync_profiles: Arc<RwLock<Vec<putio::sync::SyncProfile>>> =
        Arc::new(RwLock::new(Vec::new()));
    let pending_local_clear: Rc<RefCell<Option<i32>>> = Rc::new(RefCell::new(None));
    // UI-thread-only state
    let current_folder: Rc<RefCell<u64>> = Rc::new(RefCell::new(0));
    let path_stack: Rc<RefCell<Vec<(u64, String)>>> =
        Rc::new(RefCell::new(vec![(0u64, "put.io".to_string())]));
    let oauth_flow: Rc<RefCell<OauthFlow>> = Rc::new(RefCell::new(OauthFlow::default()));

    let visible_model = Rc::new(VecModel::from(Vec::<FileItem>::new()));
    let path_model = Rc::new(VecModel::from(Vec::<PathSegment>::new()));
    let metadata_model = Rc::new(VecModel::from(Vec::<MetadataItem>::new()));
    app.set_visible_items(ModelRc::from(visible_model.clone()));
    app.set_path_segments(ModelRc::from(path_model.clone()));
    app.set_metadata_items(ModelRc::from(metadata_model.clone()));
    app.set_detail_item(empty_file_item());

    // Wire the request_refresh callback (runs on UI thread)
    app.on_request_refresh({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let visible_model = visible_model.clone();
        let path_model = path_model.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let tree = tree.read().unwrap();
            let folder_id = *current_folder.borrow();
            let location = location_text(&path_stack.borrow());
            let mut rows = children_for_folder(&tree, folder_id);
            let query = app.get_files_query().to_lowercase();
            if !query.is_empty() {
                rows.retain(|e| e.file.name.to_lowercase().contains(&query));
            }
            let descending = app.get_files_sort_descending();
            match app.get_files_sort() {
                1 => {
                    rows.sort_by(|a, b| a.file.name.to_lowercase().cmp(&b.file.name.to_lowercase()))
                }
                2 => rows.sort_by(|a, b| a.aggregate_size.cmp(&b.aggregate_size)),
                3 => rows.sort_by(|a, b| {
                    a.file
                        .created_at
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.file.created_at.as_deref().unwrap_or(""))
                }),
                _ => rows.sort_by(|a, b| {
                    a.file
                        .updated_at
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.file.updated_at.as_deref().unwrap_or(""))
                }),
            }
            if descending {
                rows.reverse();
            }

            let mut folder_count = 0i32;
            let mut file_count = 0i32;
            for r in &rows {
                if r.file.file_type == "FOLDER" {
                    folder_count += 1;
                } else {
                    file_count += 1;
                }
            }

            visible_model.set_vec(
                rows.iter()
                    .map(|e| put_to_file_item(e, &location))
                    .collect::<Vec<_>>(),
            );
            path_model.set_vec(
                path_stack
                    .borrow()
                    .iter()
                    .skip(1)
                    .map(|(id, name)| PathSegment {
                        id: truncate_id(*id),
                        name: name.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            );

            app.set_folder_count(folder_count);
            app.set_file_count(file_count);
            app.set_has_parent(path_stack.borrow().len() > 1);

            let mut total_size: u64 = 0;
            let mut tf: u64 = 0;
            let mut tfo: u64 = 0;
            count_in_node(&tree.root, &mut total_size, &mut tf, &mut tfo);
            app.set_total_label(format!("TOTAL · {}", format_size(total_size)).into());
        }
    });

    // Helper: refresh from UI thread.
    let request_refresh_now = {
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.invoke_request_refresh();
            }
        }
    };

    app.on_settings_refresh({
        let weak = app.as_weak();
        let config = config.clone();
        let files_store = files_store.clone();
        let file_state = file_state.clone();
        let sync_profiles = sync_profiles.clone();
        move || {
            let Some(app) = weak.upgrade() else { return; };
            let local_key = config.tmdb_local_key();
            let putio_key = config.tmdb_putio_key();
            let tmdb_source = source_to_index(&config.tmdb_source());
            let mpv_custom_path = config.mpv_path();
            let detection = mpv::detect::MpvDetection::run((!mpv_custom_path.is_empty()).then(|| PathBuf::from(&mpv_custom_path)));
            let active = mpv::active_path(&config, &detection);
            let (hint, can_open_link, _) = install_hint();

            app.set_tmdb_local_key(local_key.into());
            app.set_tmdb_putio_key(putio_key.into());
            app.set_tmdb_source(tmdb_source);

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

            let state = file_state.read().unwrap();
            app.set_sync_known_count(state.entries().len() as i32);
            app.set_sync_played_count(count_played(state.entries()) as i32);
            drop(state);

            app.set_mpv_source(source_to_index(&config.mpv_source()));
            app.set_mpv_show_managed(cfg!(target_os = "windows"));
            if !cfg!(target_os = "windows") && config.mpv_source() == "managed" {
                app.set_mpv_source(0);
            }
            app.set_mpv_custom_path(mpv_custom_path.into());
            app.set_mpv_system_available(detection.system.is_some());
            app.set_mpv_custom_available(detection.custom.is_some());
            app.set_mpv_managed_available(detection.managed.is_some());
            app.set_mpv_system_path(detection.system.as_deref().map(path_label).unwrap_or_default().into());
            app.set_mpv_managed_path(detection.managed.as_deref().map(path_label).unwrap_or_default().into());
            app.set_mpv_active_path(active.as_deref().map(path_label).unwrap_or_default().into());
            app.set_mpv_install_hint(if active.is_none() { hint.into() } else { "".into() });
            app.set_mpv_can_open_install_link(can_open_link);

            let config_path = config.path();
            let file_state_path = file_state.read().unwrap().path();
            let files_path = files_store.path();
            let base = storage::config_dir().unwrap_or_else(|_| PathBuf::from(""));
            let rows = vec![
                LocalDataRow {
                    name: "App configuration".into(),
                    desc: "OAuth token, TMDB keys, MPV path and selected sync profile.".into(),
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
                    desc: "File to movie/TV show ID mappings. Not implemented in the Rust app yet.".into(),
                    path: path_label(&base.join("matched.json")).into(),
                    enabled: false,
                },
                LocalDataRow {
                    name: "TMDB data".into(),
                    desc: "Cached movie and TV show metadata. Not implemented in the Rust app yet.".into(),
                    path: path_label(&base.join("tmdb.json")).into(),
                    enabled: false,
                },
                LocalDataRow {
                    name: "TVMaze data".into(),
                    desc: "Cached TV show metadata from TVMaze. Not implemented in the Rust app yet.".into(),
                    path: path_label(&base.join("tvmaze.json")).into(),
                    enabled: false,
                },
            ];
            app.set_local_data_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        }
    });

    app.invoke_settings_refresh();

    app.on_tmdb_source_changed({
        let weak = app.as_weak();
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
        let weak = app.as_weak();
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
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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

    app.on_sync_profile_selected({
        let weak = app.as_weak();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_sync_existing_index(index);
            }
        }
    });

    app.on_sync_refresh_profiles({
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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
        let weak = app.as_weak();
        let cfg = config.clone();
        let client = client.clone();
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
        let weak = app.as_weak();
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

    app.on_mpv_source_changed({
        let weak = app.as_weak();
        let cfg = config.clone();
        move |source| {
            if source == 2 {
                if let Some(app) = weak.upgrade() {
                    app.set_mpv_source(source_to_index(&cfg.mpv_source()));
                    app.invoke_settings_refresh();
                }
                return;
            }
            if let Err(e) = cfg.set_mpv_source(mpv_source_from_index(source)) {
                warn!("save MPV source: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.invoke_settings_refresh();
            }
        }
    });

    app.on_mpv_custom_path_edited({
        let weak = app.as_weak();
        let cfg = config.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if let Err(e) = cfg.set_mpv_path(&app.get_mpv_custom_path()) {
                warn!("save MPV custom path: {e}");
            }
            app.invoke_settings_refresh();
        }
    });

    app.on_mpv_browse_custom({
        let weak = app.as_weak();
        let cfg = config.clone();
        let rt = rt.clone();
        move || {
            let weak = weak.clone();
            let cfg = cfg.clone();
            rt.spawn(async move {
                if let Some(file) = rfd::AsyncFileDialog::new().pick_file().await {
                    let path = file.path().to_string_lossy().to_string();
                    let _ = cfg.set_mpv_path(&path);
                    let _ = cfg.set_mpv_source("custom");
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        app.set_mpv_custom_path(path.into());
                        app.invoke_settings_refresh();
                    });
                }
            });
        }
    });

    app.on_mpv_open_install_link({
        move || {
            let (_, can_open, url) = install_hint();
            if can_open && !url.is_empty() {
                if let Err(e) = open::that(url) {
                    warn!("could not open install link: {e}");
                }
            }
        }
    });

    app.on_local_data_clear({
        let weak = app.as_weak();
        let pending = pending_local_clear.clone();
        let file_state = file_state.clone();
        let files_store = files_store.clone();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let request_refresh_now = request_refresh_now.clone();
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
                            request_refresh_now();
                            "Cleared cached files data.".to_string()
                        }
                        Err(e) => format!("Could not clear files data: {e}"),
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
        let weak = app.as_weak();
        let pending = pending_local_clear.clone();
        move || {
            *pending.borrow_mut() = None;
            if let Some(app) = weak.upgrade() {
                app.set_confirm_open(false);
            }
        }
    });

    app.on_local_data_confirm_clear({
        let weak = app.as_weak();
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

    // Startup token check
    {
        let weak = app.as_weak();
        let cfg = config.clone();
        let files_store = files_store.clone();
        let client = client.clone();
        let tree = tree.clone();
        let token = config.oauth_token();
        let sync_profiles = sync_profiles.clone();

        if token.is_empty() {
            app.set_view(VIEW_SPLASH);
        } else {
            // Pre-load cached tree
            if let Ok(t) = files_store.read_tree() {
                *tree.write().unwrap() = t;
            }

            rt.spawn(async move {
                let valid = oauth::check_token_validity(&client, &token)
                    .await
                    .unwrap_or(false);
                if !valid {
                    info!("stored token invalid, clearing");
                    let _ = cfg.clear_oauth_token();
                    let _ = weak.upgrade_in_event_loop(|app| {
                        app.set_view(VIEW_SPLASH);
                    });
                    return;
                }
                let _ = weak.upgrade_in_event_loop(|app| {
                    app.set_view(VIEW_FILES);
                    app.invoke_request_refresh();
                });
                match putio::config_kv::get(&client, &token, putio::config_kv::TMDB_KEY).await {
                    Ok(value) => {
                        let _ = cfg.set_tmdb_putio_key(&value);
                    }
                    Err(e) => warn!("refresh put.io TMDB key failed: {e}"),
                }
                match putio::sync::list_profiles(&client, &token, &cfg).await {
                    Ok(profiles) => {
                        *sync_profiles.write().unwrap() = profiles;
                    }
                    Err(e) => warn!("list sync profiles failed: {e}"),
                }
                let _ = weak.upgrade_in_event_loop(|app| {
                    app.invoke_settings_refresh();
                });

                match putio::files::build_tree(client, token).await {
                    Ok(new_tree) => {
                        info!(
                            "tree refresh done: {} folders, {} files",
                            new_tree.total_folders, new_tree.total_files
                        );
                        if let Err(e) = files_store.write_tree(&new_tree) {
                            error!("write tree: {e}");
                        }
                        *tree.write().unwrap() = new_tree;
                        let _ = weak.upgrade_in_event_loop(|app| {
                            app.invoke_request_refresh();
                        });
                    }
                    Err(e) => error!("tree refresh failed: {e}"),
                }
            });
        }
    }

    // Sign in
    app.on_sign_in({
        let weak = app.as_weak();
        let client = client.clone();
        let cfg = config.clone();
        let rt = rt.clone();
        let oauth_flow = oauth_flow.clone();
        let tree = tree.clone();
        let files_store = files_store.clone();
        let sync_profiles = sync_profiles.clone();
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
                            let _ = weak_inner.upgrade_in_event_loop(|app| {
                                app.set_view(VIEW_FILES);
                                app.invoke_request_refresh();
                            });
                            match putio::config_kv::get(&client, &token, putio::config_kv::TMDB_KEY)
                                .await
                            {
                                Ok(value) => {
                                    let _ = cfg.set_tmdb_putio_key(&value);
                                }
                                Err(e) => warn!("refresh put.io TMDB key failed: {e}"),
                            }
                            match putio::sync::list_profiles(&client, &token, &cfg).await {
                                Ok(profiles) => {
                                    *sync_profiles.write().unwrap() = profiles;
                                }
                                Err(e) => warn!("list sync profiles failed: {e}"),
                            }
                            let _ = weak_inner.upgrade_in_event_loop(|app| {
                                app.invoke_settings_refresh();
                            });
                            match putio::files::build_tree(client.clone(), token).await {
                                Ok(new_tree) => {
                                    let _ = files_store.write_tree(&new_tree);
                                    *tree.write().unwrap() = new_tree;
                                    let _ = weak_inner.upgrade_in_event_loop(|app| {
                                        app.invoke_request_refresh();
                                    });
                                }
                                Err(e) => error!("initial tree build failed: {e}"),
                            }
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
        let weak = app.as_weak();
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
        let weak = app.as_weak();
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
        let weak = app.as_weak();
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
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_FILES);
            }
        }
    });

    app.on_logout({
        let weak = app.as_weak();
        let cfg = config.clone();
        let tree = tree.clone();
        let path_stack = path_stack.clone();
        let current_folder = current_folder.clone();
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

    app.on_files_sort_changed({
        let weak = app.as_weak();
        let r = request_refresh_now.clone();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_files_sort(index);
            }
            r();
        }
    });
    app.on_files_mode_changed({
        let weak = app.as_weak();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_files_mode(index);
            }
        }
    });
    app.on_files_sort_direction_toggled({
        let weak = app.as_weak();
        let r = request_refresh_now.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_files_sort_descending(!app.get_files_sort_descending());
            }
            r();
        }
    });
    app.on_files_query_changed({
        let r = request_refresh_now.clone();
        move || r()
    });

    app.on_files_open_item({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh_now.clone();
        move |id, item_type| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let tree_borrow = tree.read().unwrap();
            let kids = children_for_folder(&tree_borrow, *current_folder.borrow());
            let Some(entry) = kids.iter().find(|e| truncate_id(e.file.id) == id) else {
                return;
            };
            if item_type.as_str() == "folder" {
                let real_id = entry.file.id;
                let name = entry.file.name.clone();
                drop(tree_borrow);
                *current_folder.borrow_mut() = real_id;
                path_stack.borrow_mut().push((real_id, name));
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
                r();
            } else {
                let location = location_text(&path_stack.borrow());
                app.set_detail_item(put_to_file_item(entry, &location));
                app.set_detail_open(true);
            }
        }
    });

    app.on_files_go_up({
        let weak = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh_now.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut s = path_stack.borrow_mut();
            if s.len() > 1 {
                s.pop();
            }
            *current_folder.borrow_mut() = s.last().map(|e| e.0).unwrap_or(0);
            drop(s);
            app.set_detail_open(false);
            app.set_detail_item(empty_file_item());
            r();
        }
    });

    app.on_files_go_root({
        let weak = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh_now.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                path_stack.borrow_mut().truncate(1);
                *current_folder.borrow_mut() = 0;
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
                r();
            }
        }
    });

    app.on_files_go_to_path({
        let weak = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh_now.clone();
        move |index| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut s = path_stack.borrow_mut();
            let keep = (index as usize + 2).min(s.len());
            s.truncate(keep);
            *current_folder.borrow_mut() = s.last().map(|e| e.0).unwrap_or(0);
            drop(s);
            app.set_detail_open(false);
            app.set_detail_item(empty_file_item());
            r();
        }
    });

    app.on_files_close_detail({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
            }
        }
    });

    app.on_files_menu_action(|action, id| {
        info!("menu action: {action} on {id}");
    });

    app.on_metadata_toggle_item(|_| {});
    app.on_metadata_toggle_expand(|_| {});
    app.on_metadata_select_unmatched(|| {});
    app.on_metadata_clear_selection(|| {});
    app.on_metadata_criteria_changed(|| {});

    app.run()?;
    drop(rt);
    Ok(())
}
