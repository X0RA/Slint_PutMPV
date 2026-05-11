//! File browser: tree helpers, `FileItem` mapping, and file-related Slint callbacks.

use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use slint::ComponentHandle;
use tokio::runtime::Runtime;
use tracing::{info, warn};

use crate::player::PlaybackQueueItem;
use crate::putio::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};
use crate::putio::{self};
use crate::storage::file_state::FileStateEntry;
use crate::{AppWindow, FileItem, PathSegment};

use super::models::UiModels;
use super::state::UiState;
use super::toast::{self, ToastKind};
use super::util::{format_size, format_updated, truncate_id};
use super::{Services, VIEW_PLAYER};

#[derive(Clone)]
struct DisplayEntry {
    pub file: PutIoFile,
    pub aggregate_size: u64,
    pub folder_item_count: u64,
}

pub(crate) fn empty_file_item() -> FileItem {
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
        is_watched: false,
    }
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

fn find_path_to_folder(node: &DirectoryNode, id: u64) -> Option<Vec<(u64, String)>> {
    if id == 0 {
        return Some(vec![(0, "put.io".to_string())]);
    }

    for child in &node.children {
        let Some(file) = &child.file else {
            continue;
        };
        if file.id == id {
            return Some(vec![
                (0, "put.io".to_string()),
                (file.id, file.name.clone()),
            ]);
        }
        if let Some(mut path) = find_path_to_folder(child, id) {
            path.insert(1, (file.id, file.name.clone()));
            return Some(path);
        }
    }

    None
}

fn find_entry_by_id(node: &DirectoryNode, id: i32) -> Option<(DisplayEntry, Vec<(u64, String)>)> {
    fn walk(
        node: &DirectoryNode,
        id: i32,
        stack: &mut Vec<(u64, String)>,
    ) -> Option<(DisplayEntry, Vec<(u64, String)>)> {
        for child in &node.children {
            let Some(file) = &child.file else {
                continue;
            };
            if truncate_id(file.id) == id {
                return Some((
                    DisplayEntry {
                        file: file.clone(),
                        aggregate_size: node_total_size(child),
                        folder_item_count: node_item_count(child),
                    },
                    stack.clone(),
                ));
            }

            stack.push((file.id, file.name.clone()));
            if let Some(found) = walk(child, id, stack) {
                return Some(found);
            }
            stack.pop();
        }

        for file in &node.files {
            if truncate_id(file.id) == id {
                return Some((
                    DisplayEntry {
                        file: file.clone(),
                        aggregate_size: file.size,
                        folder_item_count: 0,
                    },
                    stack.clone(),
                ));
            }
        }

        None
    }

    walk(node, id, &mut vec![(0, "put.io".to_string())])
}

fn rename_entry_in_tree(tree: &mut UnifiedDirectoryTree, file_id: u64, new_name: &str) -> bool {
    fn walk(node: &mut DirectoryNode, file_id: u64, new_name: &str) -> bool {
        for child in &mut node.children {
            if let Some(f) = &mut child.file {
                if f.id == file_id {
                    f.name = new_name.to_string();
                    return true;
                }
            }
            if walk(child, file_id, new_name) {
                return true;
            }
        }
        for f in &mut node.files {
            if f.id == file_id {
                f.name = new_name.to_string();
                return true;
            }
        }
        false
    }
    if file_id == 0 {
        return false;
    }
    walk(&mut tree.root, file_id, new_name)
}

fn delete_entry_from_tree(tree: &mut UnifiedDirectoryTree, file_id: u64) -> bool {
    fn walk(node: &mut DirectoryNode, file_id: u64) -> bool {
        let before = node.children.len();
        node.children.retain(|c| {
            c.file.as_ref().is_none_or(|f| f.id != file_id)
        });
        if node.children.len() < before {
            return true;
        }
        for child in &mut node.children {
            if walk(child, file_id) {
                return true;
            }
        }
        let before = node.files.len();
        node.files.retain(|f| f.id != file_id);
        node.files.len() < before
    }
    if file_id == 0 {
        return false;
    }
    walk(&mut tree.root, file_id)
}

fn insert_folder_in_tree(
    tree: &mut UnifiedDirectoryTree,
    parent_id: u64,
    new_folder: PutIoFile,
) -> bool {
    fn walk(node: &mut DirectoryNode, parent_id: u64, new_folder: PutIoFile) -> bool {
        if let Some(f) = &node.file {
            if f.id == parent_id {
                node.children.push(DirectoryNode {
                    file: Some(new_folder),
                    children: vec![],
                    files: vec![],
                });
                return true;
            }
        } else if parent_id == 0 {
            node.children.push(DirectoryNode {
                file: Some(new_folder),
                children: vec![],
                files: vec![],
            });
            return true;
        }
        for child in &mut node.children {
            if walk(child, parent_id, new_folder.clone()) {
                return true;
            }
        }
        false
    }
    walk(&mut tree.root, parent_id, new_folder)
}

fn reconcile_path_stack(tree: &UnifiedDirectoryTree, stack: &mut Vec<(u64, String)>) -> bool {
    let original = stack.clone();
    if stack.is_empty() {
        stack.push((0, "put.io".to_string()));
    }

    while stack.len() > 1 {
        let folder_id = stack.last().map(|entry| entry.0).unwrap_or(0);
        if find_node_by_id(&tree.root, folder_id).is_some() {
            break;
        }
        stack.pop();
    }

    let folder_id = stack.last().map(|entry| entry.0).unwrap_or(0);
    if find_node_by_id(&tree.root, folder_id).is_none() {
        stack.clear();
        stack.push((0, "put.io".to_string()));
    }

    *stack != original
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

fn search_entries(
    tree: &UnifiedDirectoryTree,
    query: &str,
) -> Vec<(DisplayEntry, Vec<(u64, String)>)> {
    fn walk(
        node: &DirectoryNode,
        query: &str,
        stack: &mut Vec<(u64, String)>,
        out: &mut Vec<(DisplayEntry, Vec<(u64, String)>)>,
    ) {
        for child in &node.children {
            let Some(file) = &child.file else {
                continue;
            };
            if file.name.to_lowercase().contains(query) {
                out.push((
                    DisplayEntry {
                        file: file.clone(),
                        aggregate_size: node_total_size(child),
                        folder_item_count: node_item_count(child),
                    },
                    stack.clone(),
                ));
            }

            stack.push((file.id, file.name.clone()));
            walk(child, query, stack, out);
            stack.pop();
        }

        for file in &node.files {
            if file.name.to_lowercase().contains(query) {
                out.push((
                    DisplayEntry {
                        file: file.clone(),
                        aggregate_size: file.size,
                        folder_item_count: 0,
                    },
                    stack.clone(),
                ));
            }
        }
    }

    let mut out = Vec::new();
    walk(
        &tree.root,
        query,
        &mut vec![(0, "put.io".to_string())],
        &mut out,
    );
    out
}

fn sort_display_rows(rows: &mut [(DisplayEntry, String)], sort: i32, descending: bool) {
    match sort {
        1 => rows.sort_by(|a, b| {
            a.0.file
                .name
                .to_lowercase()
                .cmp(&b.0.file.name.to_lowercase())
        }),
        2 => rows.sort_by(|a, b| a.0.aggregate_size.cmp(&b.0.aggregate_size)),
        3 => rows.sort_by(|a, b| {
            a.0.file
                .created_at
                .as_deref()
                .unwrap_or("")
                .cmp(b.0.file.created_at.as_deref().unwrap_or(""))
        }),
        _ => rows.sort_by(|a, b| {
            a.0.file
                .updated_at
                .as_deref()
                .unwrap_or("")
                .cmp(b.0.file.updated_at.as_deref().unwrap_or(""))
        }),
    }
    if descending {
        rows.reverse();
    }
}

fn put_to_file_item(
    entry: &DisplayEntry,
    location: &str,
    file_state: &std::collections::BTreeMap<String, FileStateEntry>,
) -> FileItem {
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
        is_watched: file_state
            .get(&file.id.to_string())
            .map(|entry| entry.is_completed())
            .unwrap_or(false),
    }
}

fn queue_item_from_file(file: &PutIoFile) -> PlaybackQueueItem {
    PlaybackQueueItem {
        file_id: file.id,
        title: file.name.clone(),
        meta: format_size(file.size),
    }
}

fn files_playback_queue(
    app: &AppWindow,
    tree: &UnifiedDirectoryTree,
    current_folder: u64,
    selected_id: i32,
) -> Option<(Vec<PlaybackQueueItem>, u64)> {
    let selected = children_for_folder(tree, current_folder)
        .into_iter()
        .find(|entry| truncate_id(entry.file.id) == selected_id)
        .or_else(|| find_entry_by_id(&tree.root, selected_id).map(|(entry, _)| entry))?;

    let mut rows = if app.get_files_query().is_empty() {
        let location = String::new();
        children_for_folder(tree, current_folder)
            .into_iter()
            .map(|entry| (entry, location.clone()))
            .collect::<Vec<_>>()
    } else {
        let query = app.get_files_query().to_lowercase();
        search_entries(tree, &query)
            .into_iter()
            .map(|(entry, stack)| (entry, location_text(&stack)))
            .collect::<Vec<_>>()
    };
    sort_display_rows(
        &mut rows,
        app.get_files_sort(),
        app.get_files_sort_descending(),
    );

    let mut queue = rows
        .into_iter()
        .filter(|(entry, _)| entry.file.file_type == "VIDEO")
        .map(|(entry, _)| queue_item_from_file(&entry.file))
        .collect::<Vec<_>>();

    if !queue.iter().any(|item| item.file_id == selected.file.id) {
        queue = vec![queue_item_from_file(&selected.file)];
    }

    Some((queue, selected.file.id))
}

fn location_text(stack: &[(u64, String)]) -> String {
    stack
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

pub(crate) fn install(
    app: &AppWindow,
    state: &UiState,
    models: &UiModels,
    services: &Services,
    request_refresh: Rc<dyn Fn()>,
    rt: &Arc<Runtime>,
    embedded_player: &crate::player::EmbeddedPlayer,
) {
    let visible_model = models.visible.clone();
    let path_model = models.path.clone();
    let tree = state.tree.clone();
    let current_folder = state.current_folder.clone();
    let path_stack = state.path_stack.clone();
    let files_refreshing = state.files_refreshing.clone();
    let config = services.config.clone();
    let client = services.client.clone();
    let files_store = services.files_store.clone();
    let file_state = services.file_state.clone();
    let watch_sync = services.watch_sync.clone();

    app.on_request_refresh({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let visible_model = visible_model.clone();
        let path_model = path_model.clone();
        let file_state = file_state.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let tree = tree.read().unwrap();
            let path_changed = {
                let mut stack = path_stack.borrow_mut();
                reconcile_path_stack(&tree, &mut stack)
            };
            let folder_id = path_stack.borrow().last().map(|entry| entry.0).unwrap_or(0);
            *current_folder.borrow_mut() = folder_id;
            if path_changed {
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
            }
            let query = app.get_files_query().to_lowercase();
            let file_state_entries = file_state.read().unwrap().entries().clone();
            let mut rows = if query.is_empty() {
                let location = location_text(&path_stack.borrow());
                children_for_folder(&tree, folder_id)
                    .into_iter()
                    .map(|entry| (entry, location.clone()))
                    .collect::<Vec<_>>()
            } else {
                search_entries(&tree, &query)
                    .into_iter()
                    .map(|(entry, stack)| (entry, location_text(&stack)))
                    .collect::<Vec<_>>()
            };
            sort_display_rows(
                &mut rows,
                app.get_files_sort(),
                app.get_files_sort_descending(),
            );

            let mut folder_count = 0i32;
            let mut file_count = 0i32;
            for (entry, _) in &rows {
                if entry.file.file_type == "FOLDER" {
                    folder_count += 1;
                } else {
                    file_count += 1;
                }
            }

            visible_model.set_vec(
                rows.iter()
                    .map(|(entry, location)| put_to_file_item(entry, location, &file_state_entries))
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

    let r = request_refresh.clone();
    app.on_files_sort_changed({
        let weak = app.as_weak();
        let cfg = config.clone();
        let r = r.clone();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_files_sort(index);
            }
            if let Err(e) = cfg.set_files_sort(index) {
                warn!("save files sort preference: {e}");
            }
            r();
        }
    });
    app.on_files_mode_changed({
        let weak = app.as_weak();
        let cfg = config.clone();
        move |index| {
            if let Some(app) = weak.upgrade() {
                app.set_files_mode(index);
            }
            if let Err(e) = cfg.set_files_mode(index) {
                warn!("save files mode preference: {e}");
            }
        }
    });
    app.on_files_sort_direction_toggled({
        let weak = app.as_weak();
        let cfg = config.clone();
        let r = request_refresh.clone();
        move || {
            let mut descending = true;
            if let Some(app) = weak.upgrade() {
                descending = !app.get_files_sort_descending();
                app.set_files_sort_descending(descending);
            }
            if let Err(e) = cfg.set_files_sort_descending(descending) {
                warn!("save files sort direction preference: {e}");
            }
            r();
        }
    });
    app.on_files_query_changed({
        let r = request_refresh.clone();
        move || r()
    });

    app.on_files_refresh({
        let weak = app.as_weak();
        let client = client.clone();
        let config = config.clone();
        let files_store = files_store.clone();
        let tree = tree.clone();
        let rt = rt.clone();
        let files_refreshing = files_refreshing.clone();
        move || {
            if files_refreshing.swap(true, Ordering::Relaxed) {
                return;
            }

            let token = config.oauth_token();
            if token.is_empty() {
                files_refreshing.store(false, Ordering::Relaxed);
                return;
            }

            let weak = weak.clone();
            let client = client.clone();
            let files_store = files_store.clone();
            let tree = tree.clone();
            let files_refreshing = files_refreshing.clone();
            rt.spawn(async move {
                match putio::files::build_tree(client, token).await {
                    Ok(new_tree) => {
                        info!(
                            "manual tree refresh done: {} folders, {} files",
                            new_tree.total_folders, new_tree.total_files
                        );
                        if let Err(e) = files_store.write_tree(&new_tree) {
                            tracing::error!("write tree: {e}");
                        }
                        *tree.write().unwrap() = new_tree;
                        let _ = weak.upgrade_in_event_loop(|app| {
                            app.invoke_request_refresh();
                            app.invoke_metadata_criteria_changed();
                            app.invoke_auto_metadata_fetch_after_refresh();
                        });
                    }
                    Err(e) => warn!("manual tree refresh failed: {e}"),
                }
                files_refreshing.store(false, Ordering::Relaxed);
            });
        }
    });

    app.on_files_open_item({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh.clone();
        let file_state = file_state.clone();
        move |id, item_type| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let tree_borrow = tree.read().unwrap();
            let query_active = !app.get_files_query().is_empty();
            let found = if query_active {
                find_entry_by_id(&tree_borrow.root, id)
            } else {
                let current_path = path_stack.borrow().clone();
                children_for_folder(&tree_borrow, *current_folder.borrow())
                    .into_iter()
                    .find(|entry| truncate_id(entry.file.id) == id)
                    .map(|entry| (entry, current_path))
                    .or_else(|| find_entry_by_id(&tree_borrow.root, id))
            };
            let Some((entry, location_stack)) = found else {
                return;
            };
            if item_type.as_str() == "folder" {
                let real_id = entry.file.id;
                let folder_path =
                    find_path_to_folder(&tree_borrow.root, real_id).unwrap_or_else(|| {
                        let mut path = location_stack.clone();
                        path.push((real_id, entry.file.name.clone()));
                        path
                    });
                drop(tree_borrow);
                *current_folder.borrow_mut() = real_id;
                *path_stack.borrow_mut() = folder_path;
                app.set_files_query("".into());
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
                r();
            } else {
                let location = location_text(&location_stack);
                let file_state_entries = file_state.read().unwrap().entries().clone();
                app.set_detail_item(put_to_file_item(&entry, &location, &file_state_entries));
                app.set_detail_open(true);
            }
        }
    });

    app.on_files_go_up({
        let weak = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let r = request_refresh.clone();
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
        let r = request_refresh.clone();
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
        let r = request_refresh.clone();
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

    let embedded_player = embedded_player.clone();
    app.on_files_menu_action({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let embedded_player = embedded_player.clone();
        let watch_sync = watch_sync.clone();
        let request_refresh = request_refresh.clone();
        let file_state = file_state.clone();
        let client = client.clone();
        let config = config.clone();
        let rt = rt.clone();
        move |action, id| {
            info!("menu action: {action} on {id}");
            let Some(app) = weak.upgrade() else {
                return;
            };

            let tree_borrow = tree.read().unwrap();
            match action.as_str() {
                "open-folder" => {
                    let Some((entry, _)) = find_entry_by_id(&tree_borrow.root, id) else {
                        return;
                    };
                    if entry.file.file_type != "FOLDER" {
                        return;
                    }
                    let real_id = entry.file.id;
                    let folder_path =
                        find_path_to_folder(&tree_borrow.root, real_id).unwrap_or_else(|| {
                            let mut path = path_stack.borrow().clone();
                            path.push((real_id, entry.file.name.clone()));
                            path
                        });
                    drop(tree_borrow);
                    *current_folder.borrow_mut() = real_id;
                    *path_stack.borrow_mut() = folder_path;
                    app.set_files_query("".into());
                    app.set_detail_open(false);
                    app.set_detail_item(empty_file_item());
                    request_refresh();
                }
                "watched" => {
                    let Some((entry, location_stack)) =
                        find_entry_by_id(&tree_borrow.root, id)
                    else {
                        return;
                    };
                    let file_id = entry.file.id;
                    let current = {
                        let file_state_entries = file_state.read().unwrap();
                        file_state_entries
                            .entries()
                            .get(&file_id.to_string())
                            .map(|entry| entry.is_completed())
                            .unwrap_or(false)
                    };
                    drop(tree_borrow);
                    watch_sync.mark_watched(file_id, !current);
                    request_refresh();
                    if app.get_detail_open() {
                        let entries = file_state.read().unwrap().entries().clone();
                        app.set_detail_item(put_to_file_item(
                            &entry,
                            &location_text(&location_stack),
                            &entries,
                        ));
                    }
                    app.invoke_media_refresh();
                    app.invoke_settings_refresh();
                }
                "play" => {
                    let Some((queue, file_id)) =
                        files_playback_queue(&app, &tree_borrow, *current_folder.borrow(), id)
                    else {
                        app.set_player_title("Could not find the selected media file.".into());
                        app.set_view(VIEW_PLAYER);
                        return;
                    };
                    drop(tree_borrow);
                    embedded_player.play_queue(&app, queue, file_id);
                }
                "rename" => {
                    let Some((entry, _)) = find_entry_by_id(&tree_borrow.root, id) else {
                        return;
                    };
                    let file_id = entry.file.id;
                    let old_name = entry.file.name.clone();
                    drop(tree_borrow);

                    let weak = weak.clone();
                    let client = client.clone();
                    let config = config.clone();
                    let tree = tree.clone();
                    rt.spawn(async move {
                        let new_name = tokio::task::spawn_blocking({
                            let old_name = old_name.clone();
                            move || {
                                std::process::Command::new("zenity")
                                    .args([
                                        "--entry",
                                        "--title=Rename",
                                        "--text=Enter new name:",
                                        &format!("--entry-text={old_name}"),
                                    ])
                                    .output()
                                    .ok()
                                    .filter(|o| o.status.success())
                                    .and_then(|o| {
                                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                        if s.is_empty() { None } else { Some(s) }
                                    })
                            }
                        })
                        .await
                        .ok()
                        .flatten();

                        let Some(new_name) = new_name else {
                            return;
                        };
                        let token = config.oauth_token();
                        if token.is_empty() {
                            return;
                        }
                        match putio::folders::rename_file(&client, &token, file_id, &new_name).await
                        {
                            Ok(()) => {
                                let tree = tree.clone();
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    rename_entry_in_tree(&mut tree.write().unwrap(), file_id, &new_name);
                                    app.invoke_request_refresh();
                                    toast::show(&app, ToastKind::Success, "Renamed", format!("Renamed to \"{new_name}\""));
                                });
                            }
                            Err(e) => {
                                warn!("rename failed: {e}");
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    toast::show(&app, ToastKind::Error, "Rename failed", e.to_string());
                                });
                            }
                        }
                    });
                }
                "delete" => {
                    let Some((entry, _)) = find_entry_by_id(&tree_borrow.root, id) else {
                        return;
                    };
                    let file_id = entry.file.id;
                    let file_name = entry.file.name.clone();
                    drop(tree_borrow);

                    let confirmed = rfd::AsyncMessageDialog::new()
                        .set_title("Delete")
                        .set_description(format!(
                            "Are you sure you want to delete \"{file_name}\"?"
                        ))
                        .set_level(rfd::MessageLevel::Warning)
                        .set_buttons(rfd::MessageButtons::OkCancel)
                        .show();

                    let weak = weak.clone();
                    let client = client.clone();
                    let config = config.clone();
                    let tree = tree.clone();
                    rt.spawn(async move {
                        if !matches!(confirmed.await, rfd::MessageDialogResult::Ok) {
                            return;
                        }
                        let token = config.oauth_token();
                        if token.is_empty() {
                            return;
                        }
                        match putio::folders::delete_files(&client, &token, &[file_id]).await {
                            Ok(()) => {
                                let tree = tree.clone();
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    delete_entry_from_tree(&mut tree.write().unwrap(), file_id);
                                    app.set_detail_open(false);
                                    app.set_detail_item(empty_file_item());
                                    app.invoke_request_refresh();
                                });
                            }
                            Err(e) => warn!("delete failed: {e}"),
                        }
                    });
                }
                "download" => {
                    // TODO: implement download with file save dialog
                }
                _ => {}
            }
        }
    });

    app.on_files_context_menu_action({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let client = client.clone();
        let config = config.clone();
        let rt = rt.clone();
        move |action| {
            info!("context menu action: {action}");

            match action.as_str() {
                "new-folder" => {
                    let parent_id = *current_folder.borrow();
                    let weak = weak.clone();
                    let client = client.clone();
                    let config = config.clone();
                    let tree = tree.clone();
                    rt.spawn(async move {
                        let folder_name = tokio::task::spawn_blocking(|| {
                            std::process::Command::new("zenity")
                                .args([
                                    "--entry",
                                    "--title=New Folder",
                                    "--text=Enter folder name:",
                                ])
                                .output()
                                .ok()
                                .filter(|o| o.status.success())
                                .and_then(|o| {
                                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                    if s.is_empty() { None } else { Some(s) }
                                })
                        })
                        .await
                        .ok()
                        .flatten();

                        let Some(name) = folder_name else {
                            return;
                        };
                        let token = config.oauth_token();
                        if token.is_empty() {
                            return;
                        }
                        match putio::folders::create_folder(&client, &token, &name, parent_id).await
                        {
                            Ok(new_folder) => {
                                let tree = tree.clone();
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    insert_folder_in_tree(
                                        &mut tree.write().unwrap(),
                                        parent_id,
                                        new_folder,
                                    );
                                    app.invoke_request_refresh();
                                    toast::show(&app, ToastKind::Success, "Folder created", format!("Created \"{name}\""));
                                });
                            }
                            Err(e) => {
                                warn!("create folder failed: {e}");
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    toast::show(&app, ToastKind::Error, "Create folder failed", e.to_string());
                                });
                            }
                        }
                    });
                }
                _ => {}
            }
        }
    });
}
