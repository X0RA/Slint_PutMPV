//! File browser: tree helpers, `FileItem` mapping, and file-related Slint callbacks.

use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use slint::ComponentHandle;
use tokio::runtime::Runtime;
use tracing::{info, warn};

use crate::putio::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};
use crate::putio::{self};
use crate::{AppWindow, FileItem, PathSegment};

use super::models::UiModels;
use super::state::UiState;
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
        is_watched: false,
    }
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
            let descending = app.get_files_sort_descending();
            match app.get_files_sort() {
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
                    .map(|(entry, location)| put_to_file_item(entry, location))
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
                app.set_detail_item(put_to_file_item(&entry, &location));
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
        let embedded_player = embedded_player.clone();
        move |action, id| {
            info!("menu action: {action} on {id}");
            if action.as_str() != "play" {
                return;
            }

            let Some(app) = weak.upgrade() else {
                return;
            };

            let tree_borrow = tree.read().unwrap();
            let found = children_for_folder(&tree_borrow, *current_folder.borrow())
                .into_iter()
                .find(|entry| truncate_id(entry.file.id) == id)
                .or_else(|| find_entry_by_id(&tree_borrow.root, id).map(|(entry, _)| entry));
            let Some(entry) = found else {
                app.set_player_title("Could not find the selected media file.".into());
                app.set_view(VIEW_PLAYER);
                return;
            };
            let file_id = entry.file.id;
            let title = entry.file.name.clone();
            drop(tree_borrow);

            embedded_player.play(&app, file_id, title);
        }
    });
}
