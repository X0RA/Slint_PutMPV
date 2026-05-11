use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, VecModel};
use tokio::runtime::Runtime;

use crate::app_ui::util::{format_size, truncate_id};
use crate::putio;
use crate::putio::transfers::PutIoTransfer;
use crate::{AppWindow, TransferItem};

use super::toast::{self, ToastKind};
use super::{Services, UiState, VIEW_FILES};

// id -> last-known status string from /transfers/list
type StatusCache = Arc<Mutex<HashMap<u64, String>>>;

pub(crate) fn install(app: &AppWindow, services: &Services, state: &UiState, rt: &Arc<Runtime>) {
    let refreshing = Arc::new(AtomicBool::new(false));
    let pending_cancel: Rc<RefCell<Option<(u64, String)>>> = Rc::new(RefCell::new(None));
    let status_cache: StatusCache = Arc::new(Mutex::new(HashMap::new()));

    app.on_transfers_refresh({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        let refreshing = refreshing.clone();
        let status_cache = status_cache.clone();
        move || {
            refresh_transfers(
                weak.clone(),
                client.clone(),
                config.oauth_token(),
                rt.clone(),
                refreshing.clone(),
                status_cache.clone(),
            );
        }
    });

    app.on_transfers_open_add({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_transfers_status("".into());
                app.set_transfers_add_open(true);
            }
        }
    });

    app.on_transfers_close_add({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                close_add_overlay(&app);
            }
        }
    });

    app.on_transfers_submit_magnet({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let magnet = app.get_transfers_magnet().trim().to_string();
            if magnet.is_empty() {
                app.set_transfers_status("Paste a magnet link before adding a transfer.".into());
                return;
            }
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_transfers_status("Sign in before adding transfers.".into());
                return;
            }
            app.set_transfers_busy(true);
            app.set_transfers_status("Adding transfer...".into());
            close_add_overlay(&app);

            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let (message, kind, title, body) =
                    match putio::transfers::add_url(&client, &token, &magnet).await {
                        Ok(_) => (
                            "Transfer added.".to_string(),
                            ToastKind::Success,
                            "Transfer added",
                            "The magnet transfer was sent to put.io.".to_string(),
                        ),
                        Err(e) => {
                            let error = e.to_string();
                            (
                                format!("Could not add transfer: {error}"),
                                ToastKind::Error,
                                "Could not add transfer",
                                error,
                            )
                        }
                    };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_transfers_busy(false);
                    app.set_transfers_status(message.into());
                    toast::show(&app, kind, title, body);
                    app.invoke_transfers_refresh();
                });
            });
        }
    });

    app.on_transfers_pick_torrent_file({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Torrent files", &["torrent"])
                .pick_file()
            else {
                return;
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_transfers_status("Sign in before uploading torrent files.".into());
                return;
            }
            let filename = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "transfer.torrent".to_string());
            app.set_transfers_busy(true);
            app.set_transfers_status(format!("Uploading {filename}...").into());
            close_add_overlay(&app);

            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let (message, kind, title, body) = match std::fs::read(&path) {
                    Ok(body) => {
                        match putio::transfers::upload_torrent(&client, &token, &filename, body)
                            .await
                        {
                            Ok(_) => (
                                format!("Uploaded {filename}."),
                                ToastKind::Success,
                                "Torrent uploaded",
                                format!("{filename} was sent to put.io."),
                            ),
                            Err(e) => {
                                let error = e.to_string();
                                (
                                    format!("Could not upload torrent: {error}"),
                                    ToastKind::Error,
                                    "Could not upload torrent",
                                    error,
                                )
                            }
                        }
                    }
                    Err(e) => {
                        let error = e.to_string();
                        (
                            format!("Could not read torrent file: {error}"),
                            ToastKind::Error,
                            "Could not read torrent file",
                            error,
                        )
                    }
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_transfers_busy(false);
                    app.set_transfers_status(message.into());
                    toast::show(&app, kind, title, body);
                    app.invoke_transfers_refresh();
                });
            });
        }
    });

    app.on_transfers_reannounce({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        let status_cache = status_cache.clone();
        move |id| {
            let Some(id) = parse_transfer_id(id.as_str()) else {
                return;
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_transfers_status("Sign in before reannouncing transfers.".into());
                return;
            }
            // Failed transfers can't be reannounced; the API endpoint for them is /retry.
            let is_error = status_cache
                .lock()
                .ok()
                .and_then(|cache| cache.get(&id).cloned())
                .map(|status| status == "ERROR")
                .unwrap_or(false);
            let busy_message = if is_error {
                "Retrying transfer..."
            } else {
                "Reannouncing transfer..."
            };
            app.set_transfers_status(busy_message.into());
            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message = if is_error {
                    match putio::transfers::retry(&client, &token, id).await {
                        Ok(()) => "Transfer retried.".to_string(),
                        Err(e) => format!("Could not retry transfer: {e}"),
                    }
                } else {
                    match putio::transfers::reannounce(&client, &token, id).await {
                        Ok(()) => "Transfer reannounced.".to_string(),
                        Err(e) => format!("Could not reannounce transfer: {e}"),
                    }
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_result_toast(
                        &app,
                        &message,
                        if is_error {
                            "Transfer retried"
                        } else {
                            "Transfer reannounced"
                        },
                        if is_error {
                            "Could not retry transfer"
                        } else {
                            "Could not reannounce transfer"
                        },
                    );
                    app.set_transfers_status(message.into());
                    app.invoke_transfers_refresh();
                });
            });
        }
    });

    app.on_transfers_cancel_requested({
        let weak = app.as_weak();
        let pending_cancel = pending_cancel.clone();
        move |id, name| {
            let Some(id) = parse_transfer_id(id.as_str()) else {
                return;
            };
            *pending_cancel.borrow_mut() = Some((id, name.to_string()));
            if let Some(app) = weak.upgrade() {
                app.set_transfers_cancel_name(name);
                app.set_transfers_cancel_confirm_open(true);
            }
        }
    });

    app.on_transfers_cancel_dismiss({
        let weak = app.as_weak();
        let pending_cancel = pending_cancel.clone();
        move || {
            *pending_cancel.borrow_mut() = None;
            if let Some(app) = weak.upgrade() {
                app.set_transfers_cancel_confirm_open(false);
                app.set_transfers_cancel_name("".into());
            }
        }
    });

    app.on_transfers_cancel_confirm({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        let pending_cancel = pending_cancel.clone();
        move || {
            let Some((id, name)) = pending_cancel.borrow_mut().take() else {
                return;
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_transfers_status("Sign in before canceling transfers.".into());
                return;
            }
            app.set_transfers_cancel_confirm_open(false);
            app.set_transfers_cancel_name("".into());
            app.set_transfers_status(format!("Canceling {name}...").into());
            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message = match putio::transfers::cancel(&client, &token, id).await {
                    Ok(()) => "Transfer canceled.".to_string(),
                    Err(e) => format!("Could not cancel transfer: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_result_toast(
                        &app,
                        &message,
                        "Transfer canceled",
                        "Could not cancel transfer",
                    );
                    app.set_transfers_status(message.into());
                    app.invoke_transfers_refresh();
                });
            });
        }
    });

    app.on_transfers_clear_completed({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_transfers_status("Sign in before clearing completed transfers.".into());
                return;
            }
            app.set_transfers_status("Clearing completed transfers...".into());
            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message = match putio::transfers::clean_completed(&client, &token).await {
                    Ok(()) => "Completed transfers cleared.".to_string(),
                    Err(e) => format!("Could not clear completed transfers: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_result_toast(
                        &app,
                        &message,
                        "Completed transfers cleared",
                        "Could not clear completed transfers",
                    );
                    app.set_transfers_status(message.into());
                    app.invoke_transfers_refresh();
                });
            });
        }
    });

    app.on_transfers_go_to_file({
        let weak = app.as_weak();
        let tree = state.tree.clone();
        let path_stack = state.path_stack.clone();
        move |file_id, parent_id| {
            let Ok(file_id) = file_id.as_str().parse::<u64>() else {
                return;
            };
            let parent_id = parent_id.as_str().parse::<u64>().unwrap_or(0);
            let Some(app) = weak.upgrade() else {
                return;
            };
            let path = {
                let tree = tree.read().unwrap();
                find_file_folder_path(&tree.root, file_id)
            };
            if let Some(path) = path {
                *path_stack.borrow_mut() = path;
                app.set_view(VIEW_FILES);
                app.invoke_request_refresh();
                app.invoke_files_open_item(truncate_id(file_id), "file".into());
                return;
            }

            if parent_id > 0 {
                let folder_path = {
                    let tree = tree.read().unwrap();
                    find_folder_path(&tree.root, parent_id)
                };
                if let Some(path) = folder_path {
                    *path_stack.borrow_mut() = path;
                    app.set_view(VIEW_FILES);
                    app.invoke_request_refresh();
                    app.set_transfers_status(
                        "Opened the destination folder; refresh files if the transfer just completed."
                            .into(),
                    );
                    return;
                }
            }

            app.set_transfers_status("The completed file is not in the local file cache yet.".into());
        }
    });
}

fn refresh_transfers(
    weak: slint::Weak<AppWindow>,
    client: putio::PutioClient,
    token: String,
    rt: Arc<Runtime>,
    refreshing: Arc<AtomicBool>,
    status_cache: StatusCache,
) {
    if refreshing.swap(true, Ordering::SeqCst) {
        return;
    }
    if token.is_empty() {
        refreshing.store(false, Ordering::SeqCst);
        if let Some(app) = weak.upgrade() {
            app.set_transfers_status("Sign in before viewing transfers.".into());
        }
        return;
    }
    rt.spawn(async move {
        let result = putio::transfers::list(&client, &token).await;
        let _ = weak.upgrade_in_event_loop(move |app| {
            refreshing.store(false, Ordering::SeqCst);
            match result {
                Ok(transfers) => {
                    if let Ok(mut cache) = status_cache.lock() {
                        cache.clear();
                        for t in &transfers {
                            cache.insert(t.id, t.status.clone());
                        }
                    }
                    let count = transfers.len();
                    let items = transfers
                        .iter()
                        .map(transfer_to_item)
                        .collect::<Vec<TransferItem>>();
                    app.set_transfers_items(ModelRc::from(Rc::new(VecModel::from(items))));
                    app.set_transfers_count_label(
                        format!("{count} transfer{}", if count == 1 { "" } else { "s" }).into(),
                    );
                    if app.get_transfers_status().starts_with("Could not load") {
                        app.set_transfers_status("".into());
                    }
                }
                Err(e) => {
                    app.set_transfers_status(format!("Could not load transfers: {e}").into());
                }
            }
        });
    });
}

fn close_add_overlay(app: &AppWindow) {
    app.set_transfers_add_open(false);
    app.set_transfers_magnet("".into());
}

fn parse_transfer_id(id: &str) -> Option<u64> {
    id.parse::<u64>().ok()
}

fn show_result_toast(app: &AppWindow, message: &str, success_title: &str, error_title: &str) {
    if message.starts_with("Could not") {
        toast::show(app, ToastKind::Error, error_title, message);
    } else {
        toast::show(app, ToastKind::Success, success_title, message);
    }
}

fn transfer_to_item(transfer: &PutIoTransfer) -> TransferItem {
    TransferItem {
        id: transfer.id.to_string().into(),
        file_id: if transfer.file_id > 0 {
            transfer.file_id.to_string()
        } else {
            String::new()
        }
        .into(),
        parent_id: if transfer.save_parent_id > 0 {
            transfer.save_parent_id.to_string()
        } else {
            String::new()
        }
        .into(),
        title: transfer_title(transfer).into(),
        meta: transfer_meta(transfer).into(),
        progress: (transfer.percent_done.clamp(0.0, 100.0) / 100.0) as f32,
        status_style: status_style(&transfer.status),
        file_available: transfer.file_id > 0
            && matches!(transfer.status.as_str(), "COMPLETED" | "SEEDING"),
    }
}

fn transfer_title(transfer: &PutIoTransfer) -> String {
    if !transfer.name.is_empty() {
        transfer.name.clone()
    } else if !transfer.source.is_empty() {
        transfer.source.clone()
    } else {
        format!("Transfer {}", transfer.id)
    }
}

pub(crate) fn status_style(status: &str) -> i32 {
    match status {
        "SEEDING" => 1,
        "COMPLETED" => 2,
        "ERROR" => 3,
        _ => 0,
    }
}

pub(crate) fn transfer_meta(transfer: &PutIoTransfer) -> String {
    match transfer.status.as_str() {
        "SEEDING" => format!(
            "Seeding | up: {}/s | seeded: {} of {} | seed time: {} | ratio: {:.2}",
            format_size(transfer.up_speed),
            format_size(transfer.uploaded),
            format_size(transfer.size),
            format_duration(transfer.seconds_seeding),
            transfer.current_ratio
        ),
        "COMPLETED" => {
            let mut parts = vec![
                "Completed".to_string(),
                format!(
                    "seeded: {} of {}",
                    format_size(transfer.uploaded),
                    format_size(transfer.size)
                ),
                format!("seed time: {}", format_duration(transfer.seconds_seeding)),
                format!("ratio: {:.2}", transfer.current_ratio),
            ];
            if !transfer.finished_at.is_empty() {
                parts.push(transfer.finished_at.clone());
            }
            parts.join(" | ")
        }
        "ERROR" => {
            let message = first_non_empty(&[&transfer.error_message, &transfer.tracker_message])
                .unwrap_or("Unknown error");
            format!(
                "Error | {message} | downloaded: {} of {}",
                format_size(transfer.downloaded),
                format_size(transfer.size)
            )
        }
        "IN_QUEUE" | "WAITING" => format!(
            "{} | waiting for transfer slot | peers: {}",
            human_status(&transfer.status),
            transfer.peers
        ),
        _ => {
            let eta = if transfer.estimated_time > 0 {
                format!(" | ETA: {}", format_duration(transfer.estimated_time))
            } else {
                String::new()
            };
            format!(
                "{} | down: {}/s | {} of {}{eta} | peers: {}",
                human_status(&transfer.status),
                format_size(transfer.down_speed),
                format_size(transfer.downloaded),
                format_size(transfer.size),
                transfer.peers
            )
        }
    }
}

fn first_non_empty<'a>(values: &[&'a str]) -> Option<&'a str> {
    values.iter().copied().find(|value| !value.is_empty())
}

fn human_status(status: &str) -> String {
    let lower = status.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Transfer".to_string(),
    }
}

pub(crate) fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

fn find_file_folder_path(
    node: &putio::types::DirectoryNode,
    file_id: u64,
) -> Option<Vec<(u64, String)>> {
    fn walk(
        node: &putio::types::DirectoryNode,
        file_id: u64,
        stack: &mut Vec<(u64, String)>,
    ) -> Option<Vec<(u64, String)>> {
        for file in &node.files {
            if file.id == file_id {
                return Some(stack.clone());
            }
        }
        for child in &node.children {
            let Some(folder) = &child.file else {
                continue;
            };
            stack.push((folder.id, folder.name.clone()));
            if let Some(path) = walk(child, file_id, stack) {
                return Some(path);
            }
            stack.pop();
        }
        None
    }
    walk(node, file_id, &mut vec![(0, "put.io".to_string())])
}

fn find_folder_path(
    node: &putio::types::DirectoryNode,
    folder_id: u64,
) -> Option<Vec<(u64, String)>> {
    fn walk(
        node: &putio::types::DirectoryNode,
        folder_id: u64,
        stack: &mut Vec<(u64, String)>,
    ) -> Option<Vec<(u64, String)>> {
        for child in &node.children {
            let Some(folder) = &child.file else {
                continue;
            };
            stack.push((folder.id, folder.name.clone()));
            if folder.id == folder_id {
                return Some(stack.clone());
            }
            if let Some(path) = walk(child, folder_id, stack) {
                return Some(path);
            }
            stack.pop();
        }
        None
    }
    if folder_id == 0 {
        Some(vec![(0, "put.io".to_string())])
    } else {
        walk(node, folder_id, &mut vec![(0, "put.io".to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formats_compactly() {
        assert_eq!(format_duration(36), "36s");
        assert_eq!(format_duration(671), "11m 11s");
        assert_eq!(format_duration(3_900), "1h 5m");
        assert_eq!(format_duration(90_000), "1d 1h");
    }

    #[test]
    fn status_styles_match_ui_groups() {
        assert_eq!(status_style("DOWNLOADING"), 0);
        assert_eq!(status_style("SEEDING"), 1);
        assert_eq!(status_style("COMPLETED"), 2);
        assert_eq!(status_style("ERROR"), 3);
    }
}
