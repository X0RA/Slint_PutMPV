use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use metadata::api::{EpisodeRefByFileID, MatchItemByFileID};
use metadata::tmdb::{MovieDetails, TVSeasonDetails, TVSeriesDetails};
use slint::{ModelRc, VecModel};
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

mod fileparser;
mod metadata;
mod mpv;
mod player;
mod putio;
mod storage;

use putio::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};
use putio::{oauth, PutioClient};
use storage::config::ConfigStore;
use storage::file_state::{count_played, FileStateStore};
use storage::files_store::FilesStore;
use storage::matched_store::MatchedStore;
use storage::tmdb_store::TMDBStore;
use storage::tvmaze_store::TVMazeStore;

slint::include_modules!();

const VIEW_LOADING: i32 = 0;
const VIEW_SPLASH: i32 = 1;
const VIEW_CODE: i32 = 2;
const VIEW_FILES: i32 = 3;
const VIEW_PLAYER: i32 = 7;
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
        is_watched: false,
    }
}

fn metadata_item_empty() -> MetadataItem {
    MetadataItem {
        id: -1,
        parent_id: -1,
        media_type: "".into(),
        title: "".into(),
        subtitle: "".into(),
        badge: "".into(),
        filename: "".into(),
        relative_path: "".into(),
        season: 0,
        episode: 0,
        matched: false,
        expanded: false,
        selected: false,
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

#[derive(Debug, Clone)]
struct MetadataUiState {
    rows: Vec<MetadataRow>,
    expanded: std::collections::BTreeSet<i32>,
    selected: std::collections::BTreeSet<i32>,
}

impl MetadataUiState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            expanded: std::collections::BTreeSet::new(),
            selected: std::collections::BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum MetadataRowKind {
    Show,
    Episode,
    Movie,
}

#[derive(Debug, Clone)]
struct MetadataRow {
    id: i32,
    parent_id: i32,
    kind: MetadataRowKind,
    file_id: String,
    title: String,
    subtitle: String,
    badge: String,
    filename: String,
    relative_path: String,
    season: i32,
    episode: i32,
    year: i32,
    matched: bool,
}

#[derive(Debug, Clone)]
enum MetadataFetchCandidate {
    Movie {
        file_id: String,
        title: String,
        year: i32,
    },
    Show {
        title: String,
        episodes: Vec<EpisodeRefByFileID>,
    },
}

#[derive(Debug, Default)]
struct ShowMetadataMatchOutcome {
    matched_episodes: usize,
    missed: bool,
    errors: Vec<String>,
}

fn stable_i32_id(value: &str) -> i32 {
    let mut hash: u32 = 2_166_136_261;
    for b in value.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash & 0x7fff_ffff) as i32
}

fn build_metadata_rows(
    tree: &UnifiedDirectoryTree,
    matched: &storage::matched_store::MatchedData,
) -> Vec<MetadataRow> {
    let lib = fileparser::parse_directory_tree(&tree.root);
    let mut rows = Vec::new();
    for show in lib.shows.values() {
        let show_id = stable_i32_id(&format!("show:{}", show.key));
        let show_episodes = show
            .seasons
            .values()
            .flat_map(|season| season.episodes.iter())
            .collect::<Vec<_>>();
        let episode_count = show_episodes.len();
        let season_count = show.seasons.len();
        let matched_count = show_episodes
            .iter()
            .filter(|ep| matched.tv.contains_key(&ep.file_id))
            .count();
        rows.push(MetadataRow {
            id: show_id,
            parent_id: -1,
            kind: MetadataRowKind::Show,
            file_id: String::new(),
            title: show.title.clone(),
            subtitle: format!(
                "{} season{} · {} episode{}",
                season_count,
                if season_count == 1 { "" } else { "s" },
                episode_count,
                if episode_count == 1 { "" } else { "s" }
            ),
            badge: if episode_count > 0 && matched_count == episode_count {
                "Matched".to_string()
            } else if matched_count > 0 {
                format!("{matched_count}/{episode_count}")
            } else {
                "Unmatched".to_string()
            },
            filename: String::new(),
            relative_path: String::new(),
            season: 0,
            episode: 0,
            year: 0,
            matched: episode_count > 0 && matched_count == episode_count,
        });
        for season in show.seasons.values() {
            for ep in &season.episodes {
                let ep_matched = matched.tv.contains_key(&ep.file_id);
                rows.push(MetadataRow {
                    id: stable_i32_id(&format!(
                        "episode:{}:s{}e{}",
                        ep.file_id, ep.season, ep.episode
                    )),
                    parent_id: show_id,
                    kind: MetadataRowKind::Episode,
                    file_id: ep.file_id.clone(),
                    title: if ep.episode_title.is_empty() {
                        ep.filename.clone()
                    } else {
                        ep.episode_title.clone()
                    },
                    subtitle: ep.relative_path.clone(),
                    badge: if ep_matched {
                        "Matched".to_string()
                    } else {
                        ep.quality.clone()
                    },
                    filename: ep.filename.clone(),
                    relative_path: ep.relative_path.clone(),
                    season: ep.season,
                    episode: ep.episode,
                    year: 0,
                    matched: ep_matched,
                });
            }
        }
    }
    for movie in lib.movies {
        let movie_matched = matched.movies.contains_key(&movie.file_id);
        rows.push(MetadataRow {
            id: stable_i32_id(&format!("movie:{}", movie.file_id)),
            parent_id: -1,
            kind: MetadataRowKind::Movie,
            file_id: movie.file_id,
            title: movie.title.clone(),
            subtitle: if movie.year > 0 {
                format!("{} · {}", movie.year, movie.relative_path)
            } else {
                movie.relative_path.clone()
            },
            badge: if movie_matched {
                "Matched".to_string()
            } else if movie.quality.is_empty() {
                "Unmatched".to_string()
            } else {
                movie.quality.clone()
            },
            filename: movie.filename,
            relative_path: movie.relative_path,
            season: 0,
            episode: 0,
            year: movie.year,
            matched: movie_matched,
        });
    }
    rows
}

fn row_matches_query(row: &MetadataRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {}",
        row.title, row.subtitle, row.filename, row.relative_path
    )
    .to_lowercase();
    haystack.contains(query)
}

fn metadata_fetch_candidates(state: &MetadataUiState) -> Vec<MetadataFetchCandidate> {
    let mut episodes_by_show = HashMap::<i32, Vec<EpisodeRefByFileID>>::new();
    for row in &state.rows {
        if matches!(row.kind, MetadataRowKind::Episode) {
            episodes_by_show
                .entry(row.parent_id)
                .or_default()
                .push(EpisodeRefByFileID {
                    file_id: row.file_id.clone(),
                    season: row.season,
                    episode: row.episode,
                });
        }
    }

    let mut out = Vec::new();
    for row in &state.rows {
        if !state.selected.contains(&row.id) {
            continue;
        }
        match row.kind {
            MetadataRowKind::Movie => {
                out.push(MetadataFetchCandidate::Movie {
                    file_id: row.file_id.clone(),
                    title: row.title.clone(),
                    year: row.year,
                });
            }
            MetadataRowKind::Show => {
                let episodes = episodes_by_show.remove(&row.id).unwrap_or_default();
                if !episodes.is_empty() {
                    out.push(MetadataFetchCandidate::Show {
                        title: row.title.clone(),
                        episodes,
                    });
                }
            }
            MetadataRowKind::Episode => {}
        }
    }
    out
}

fn make_initials(title: &str) -> slint::SharedString {
    let s: String = title
        .split_whitespace()
        .filter(|w| {
            w.chars()
                .next()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
        })
        .take(3)
        .map(|w| w.chars().next().unwrap().to_uppercase().to_string())
        .collect::<Vec<_>>()
        .join("");
    s.as_str().into()
}

fn poster_cache_path(poster_path: &str) -> Option<std::path::PathBuf> {
    let filename = poster_path.trim_start_matches('/');
    if filename.is_empty() {
        return None;
    }
    Some(storage::poster_cache_dir().ok()?.join(filename))
}

fn load_cached_poster(poster_path: &str) -> Option<slint::Image> {
    let path = poster_cache_path(poster_path)?;
    slint::Image::load_from_path(&path).ok()
}

async fn download_posters(poster_paths: Vec<String>) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("PutMPV/1.0")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    for poster_path in &poster_paths {
        let Some(cache_path) = poster_cache_path(poster_path) else {
            continue;
        };
        if cache_path.exists() {
            continue;
        }
        let url = format!("https://image.tmdb.org/t/p/w342{poster_path}");
        match client.get(&url).send().await {
            Ok(resp) => match resp.bytes().await {
                Ok(bytes) => {
                    if let Some(parent) = cache_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::write(&cache_path, &bytes) {
                        warn!("Failed to write poster cache {}: {e}", cache_path.display());
                    }
                }
                Err(e) => warn!("Failed to read poster bytes for {poster_path}: {e}"),
            },
            Err(e) => warn!("Failed to fetch poster {poster_path}: {e}"),
        }
    }
}

fn format_runtime(minutes: i32) -> String {
    if minutes <= 0 {
        return String::new();
    }
    let h = minutes / 60;
    let m = minutes % 60;
    match (h > 0, m > 0) {
        (true, true) => format!("{h}h {m}m"),
        (true, false) => format!("{h}h"),
        _ => format!("{m}m"),
    }
}

fn build_unmatched_candidates_from_tree(
    tree: &UnifiedDirectoryTree,
    matched: &storage::matched_store::MatchedData,
) -> Vec<MetadataFetchCandidate> {
    let lib = fileparser::parse_directory_tree(&tree.root);
    let mut existing_ids = std::collections::HashSet::<String>::new();
    collect_tree_file_ids(&tree.root, &mut existing_ids);

    let mut out = Vec::new();
    for movie in &lib.movies {
        if !existing_ids.contains(&movie.file_id) {
            continue;
        }
        if !matched.movies.contains_key(&movie.file_id) {
            out.push(MetadataFetchCandidate::Movie {
                file_id: movie.file_id.clone(),
                title: movie.title.clone(),
                year: movie.year,
            });
        }
    }
    for show in lib.shows.values() {
        let all_episodes: Vec<_> = show
            .seasons
            .values()
            .flat_map(|s| s.episodes.iter())
            .filter(|ep| existing_ids.contains(&ep.file_id))
            .collect();
        if !all_episodes
            .iter()
            .any(|ep| matched.tv.contains_key(&ep.file_id))
            && !all_episodes.is_empty()
        {
            let episodes = all_episodes
                .iter()
                .map(|ep| EpisodeRefByFileID {
                    file_id: ep.file_id.clone(),
                    season: ep.season,
                    episode: ep.episode,
                })
                .collect();
            out.push(MetadataFetchCandidate::Show {
                title: show.title.clone(),
                episodes,
            });
        }
    }
    out
}

fn collect_tree_file_ids(node: &DirectoryNode, ids: &mut std::collections::HashSet<String>) {
    for f in &node.files {
        ids.insert(f.id.to_string());
    }
    for child in &node.children {
        collect_tree_file_ids(child, ids);
    }
}

async fn match_show_metadata(
    title: &str,
    episodes: &[EpisodeRefByFileID],
    metadata_api: &metadata::MetadataAPI,
    tmdb_api: &metadata::TMDBAPI,
    tvmaze_api: &metadata::TVMazeAPI,
) -> ShowMetadataMatchOutcome {
    let mut outcome = ShowMetadataMatchOutcome::default();
    if episodes.is_empty() {
        outcome.missed = true;
        return outcome;
    }

    let mut seasons = episodes
        .iter()
        .filter_map(|ep| (ep.season > 0).then_some(ep.season))
        .collect::<BTreeSet<_>>();
    let mut tmdb_resolved = HashMap::<String, i32>::new();
    let mut tmdb_series_id = None;

    match tmdb_api.search_tv(title, 1).await {
        Ok(results) => {
            if let Some(result) = results.first() {
                tmdb_series_id = Some(result.id);
                let initial_seasons = seasons.iter().copied().collect::<Vec<_>>();
                let _ = metadata_api.seed_tv(result.id, &initial_seasons).await;

                match metadata_api
                    .resolve_tv_episodes_by_file_id(result.id, episodes)
                    .await
                {
                    Ok(resolved) => tmdb_resolved.extend(resolved),
                    Err(e) => outcome
                        .errors
                        .push(format!("{title}: TMDB episode resolution failed: {e}")),
                }

                let unresolved = unresolved_episode_refs(episodes, &tmdb_resolved);
                if !unresolved.is_empty() {
                    match metadata_api
                        .resolve_absolute_episodes(result.id, &unresolved)
                        .await
                    {
                        Ok(abs) => {
                            tmdb_resolved.extend(abs.resolved);
                            seasons.extend(abs.seasons);
                        }
                        Err(e) => outcome.errors.push(format!(
                            "{title}: TMDB absolute episode remapping failed: {e}"
                        )),
                    }
                }

                if !tmdb_resolved.is_empty() {
                    let matches = episode_match_items(&tmdb_resolved, "tmdb");
                    outcome.matched_episodes += matches.len();
                    if let Err(e) = metadata_api.bulk_store_matches_by_file_id(&matches) {
                        outcome.errors.push(format!("{title}: {e}"));
                    }
                }
            }
        }
        Err(e) => outcome
            .errors
            .push(format!("{title}: TMDB search failed: {e}")),
    }

    let unresolved_after_tmdb = unresolved_episode_refs(episodes, &tmdb_resolved);
    if !unresolved_after_tmdb.is_empty() {
        match tvmaze_api.search_shows(title).await {
            Ok(results) => {
                if let Some(result) = results.first() {
                    let tvmaze_show_id = result.show.id;
                    let mut tvmaze_resolved = match metadata_api
                        .resolve_tvmaze_episodes_by_file_id(tvmaze_show_id, &unresolved_after_tmdb)
                        .await
                    {
                        Ok(resolved) => resolved,
                        Err(e) => {
                            outcome
                                .errors
                                .push(format!("{title}: TVMaze episode resolution failed: {e}"));
                            HashMap::new()
                        }
                    };

                    let still_unresolved =
                        unresolved_episode_refs(&unresolved_after_tmdb, &tvmaze_resolved);
                    if !still_unresolved.is_empty() {
                        match metadata_api
                            .resolve_tvmaze_absolute_episodes(tvmaze_show_id, &still_unresolved)
                            .await
                        {
                            Ok(abs) => {
                                tvmaze_resolved.extend(abs.resolved);
                                seasons.extend(abs.seasons);
                            }
                            Err(e) => outcome.errors.push(format!(
                                "{title}: TVMaze absolute episode remapping failed: {e}"
                            )),
                        }
                    }

                    if !tvmaze_resolved.is_empty() {
                        let matches = episode_match_items(&tvmaze_resolved, "tvmaze");
                        outcome.matched_episodes += matches.len();
                        if let Err(e) = metadata_api.bulk_store_matches_by_file_id(&matches) {
                            outcome.errors.push(format!("{title}: {e}"));
                        }
                    }

                    let final_seasons = seasons.iter().copied().collect::<Vec<_>>();
                    let _ = metadata_api
                        .seed_tvmaze(tvmaze_show_id, &final_seasons)
                        .await;
                }
            }
            Err(e) => outcome
                .errors
                .push(format!("{title}: TVMaze search failed: {e}")),
        }
    }

    if let Some(series_id) = tmdb_series_id {
        let final_seasons = seasons.iter().copied().collect::<Vec<_>>();
        let _ = metadata_api.seed_tv(series_id, &final_seasons).await;
    }

    outcome.missed = outcome.matched_episodes == 0;
    outcome
}

fn unresolved_episode_refs(
    episodes: &[EpisodeRefByFileID],
    resolved: &HashMap<String, i32>,
) -> Vec<EpisodeRefByFileID> {
    episodes
        .iter()
        .filter(|ep| !resolved.contains_key(&ep.file_id))
        .cloned()
        .collect()
}

fn episode_match_items(resolved: &HashMap<String, i32>, source: &str) -> Vec<MatchItemByFileID> {
    resolved
        .iter()
        .filter_map(|(file_id, tmdb_id)| {
            (*tmdb_id > 0).then(|| MatchItemByFileID {
                file_id: file_id.clone(),
                kind: "episode".to_string(),
                tmdb_id: *tmdb_id,
                source: source.to_string(),
            })
        })
        .collect()
}

fn refresh_media_ui(
    app: &AppWindow,
    media_movies_model: &Rc<VecModel<MediaItem>>,
    media_shows_model: &Rc<VecModel<MediaItem>>,
    tree: &Arc<RwLock<UnifiedDirectoryTree>>,
    matched_store: &Arc<MatchedStore>,
    tmdb_store: &Arc<storage::tmdb_store::TMDBStore>,
    file_state: &Arc<RwLock<storage::file_state::FileStateStore>>,
) -> Vec<String> {
    let matched = matched_store.get_matched_snapshot().unwrap_or_default();
    let tmdb_cache = tmdb_store.get_cache_snapshot().unwrap_or_default();
    let file_state_entries = file_state.read().unwrap().entries().clone();

    // Build set of file IDs that actually exist in the tree
    let mut existing_file_ids = std::collections::HashSet::<String>::new();
    {
        let tree_guard = tree.read().unwrap();
        collect_tree_file_ids(&tree_guard.root, &mut existing_file_ids);
    }

    // Build movie details cache: tmdb_movie_id → MovieDetails
    let mut movie_details_map: HashMap<i32, MovieDetails> = HashMap::new();
    for (id_str, sub) in &tmdb_cache.movies {
        if let Ok(mid) = id_str.parse::<i32>() {
            let preferred = sub.get("details_en-US").or_else(|| {
                sub.keys()
                    .find(|k| k.starts_with("details_"))
                    .and_then(|k| sub.get(k))
            });
            if let Some(entry) = preferred {
                if let Ok(d) = serde_json::from_value::<MovieDetails>(entry.data.clone()) {
                    movie_details_map.insert(mid, d);
                }
            }
        }
    }

    // Build episode_id → series_id index from cached TV season data
    let mut episode_to_series: HashMap<i32, i32> = HashMap::new();
    // Build series details cache: series_id → TVSeriesDetails
    let mut series_details_map: HashMap<i32, TVSeriesDetails> = HashMap::new();
    for (id_str, sub) in &tmdb_cache.tv {
        if let Ok(series_id) = id_str.parse::<i32>() {
            let preferred = sub.get("details_en-US").or_else(|| {
                sub.keys()
                    .find(|k| k.starts_with("details_"))
                    .and_then(|k| sub.get(k))
            });
            if let Some(entry) = preferred {
                if let Ok(d) = serde_json::from_value::<TVSeriesDetails>(entry.data.clone()) {
                    series_details_map.insert(series_id, d);
                }
            }
            for (key, entry) in sub {
                if key.starts_with("season_") {
                    if let Ok(season) =
                        serde_json::from_value::<TVSeasonDetails>(entry.data.clone())
                    {
                        for ep in &season.episodes {
                            if ep.id > 0 {
                                episode_to_series.insert(ep.id, series_id);
                            }
                        }
                    }
                }
            }
        }
    }

    // Movies: iterate matched, filter by file existence + cached details, deduplicate by TMDB ID
    let mut movie_groups: std::collections::BTreeMap<i32, (MovieDetails, String)> =
        std::collections::BTreeMap::new();
    for (file_id, &tmdb_id) in &matched.movies {
        if !existing_file_ids.contains(file_id) {
            continue;
        }
        let Some(details) = movie_details_map.get(&tmdb_id) else {
            continue;
        };
        movie_groups
            .entry(tmdb_id)
            .or_insert_with(|| (details.clone(), file_id.clone()));
    }

    let mut missing_posters: Vec<String> = Vec::new();

    let mut movies: Vec<MediaItem> = movie_groups
        .into_values()
        .map(|(d, file_id)| {
            let year = d.release_date.get(..4).unwrap_or("").to_string();
            let rt = format_runtime(d.runtime);
            let meta = match (year.is_empty(), rt.is_empty()) {
                (true, true) => String::new(),
                (true, false) => rt,
                (false, true) => year,
                (false, false) => format!("{year} · {rt}"),
            };
            let rating = if d.vote_average > 0.0 {
                format!("{:.1}", d.vote_average)
            } else {
                String::new()
            };
            let played = file_state_entries
                .get(&file_id)
                .map(|e| e.played)
                .unwrap_or(false);
            let poster = if d.poster_path.is_empty() {
                Default::default()
            } else if let Some(img) = load_cached_poster(&d.poster_path) {
                img
            } else {
                missing_posters.push(d.poster_path.clone());
                Default::default()
            };
            MediaItem {
                title: d.title.as_str().into(),
                meta: meta.as_str().into(),
                rating: rating.as_str().into(),
                poster,
                resolution: "".into(),
                progress: if played { 1.0 } else { 0.0 },
                is_tv: false,
                initials: make_initials(&d.title),
                file_id: file_id.as_str().into(),
            }
        })
        .collect();
    movies.sort_by(|a, b| a.title.to_string().cmp(&b.title.to_string()));

    // TV shows: iterate matched TV files, resolve series via episode index, group by series_id
    // series_id → (TVSeriesDetails, matched_seasons: BTreeSet<i32>, matched_ep_count: usize, first_file_id: String)
    let mut show_groups: std::collections::BTreeMap<
        i32,
        (
            TVSeriesDetails,
            std::collections::BTreeSet<i32>,
            usize,
            String,
        ),
    > = std::collections::BTreeMap::new();
    for (file_id, &episode_id) in &matched.tv {
        if !existing_file_ids.contains(file_id) {
            continue;
        }
        let Some(&series_id) = episode_to_series.get(&episode_id) else {
            continue;
        };
        let Some(details) = series_details_map.get(&series_id) else {
            continue;
        };
        let entry = show_groups.entry(series_id).or_insert_with(|| {
            (
                details.clone(),
                std::collections::BTreeSet::new(),
                0,
                file_id.clone(),
            )
        });
        // Find which season this episode belongs to via episode_id in season cache
        let season_number = tmdb_cache.tv.get(&series_id.to_string()).and_then(|sub| {
            sub.iter()
                .filter(|(k, _)| k.starts_with("season_"))
                .find_map(|(_, e)| {
                    serde_json::from_value::<TVSeasonDetails>(e.data.clone())
                        .ok()
                        .and_then(|s| {
                            s.episodes
                                .iter()
                                .any(|ep| ep.id == episode_id)
                                .then_some(s.season_number)
                        })
                })
        });
        if let Some(sn) = season_number {
            entry.1.insert(sn);
        }
        entry.2 += 1;
    }

    let mut shows: Vec<MediaItem> = show_groups
        .into_values()
        .map(|(d, seasons, ep_count, _file_id)| {
            let year = d.first_air_date.get(..4).unwrap_or("").to_string();
            let season_count = if seasons.is_empty() {
                d.number_of_seasons.max(1)
            } else {
                seasons.len() as i32
            };
            let meta = format!(
                "{season_count}S · {ep_count}E{}",
                if year.is_empty() {
                    String::new()
                } else {
                    format!(" · {year}")
                }
            );
            let rating = if d.vote_average > 0.0 {
                format!("{:.1}", d.vote_average)
            } else {
                String::new()
            };
            let poster = if d.poster_path.is_empty() {
                Default::default()
            } else if let Some(img) = load_cached_poster(&d.poster_path) {
                img
            } else {
                missing_posters.push(d.poster_path.clone());
                Default::default()
            };
            MediaItem {
                title: d.name.as_str().into(),
                meta: meta.as_str().into(),
                rating: rating.as_str().into(),
                poster,
                resolution: "".into(),
                progress: 0.0,
                is_tv: true,
                initials: make_initials(&d.name),
                file_id: "".into(),
            }
        })
        .collect();
    shows.sort_by(|a, b| a.title.to_string().cmp(&b.title.to_string()));

    // Unmatched banner: count library items with no match or missing file
    let lib = {
        let tree_guard = tree.read().unwrap();
        fileparser::parse_directory_tree(&tree_guard.root)
    };
    let unmatched_movies = lib
        .movies
        .iter()
        .filter(|m| {
            !matched.movies.contains_key(&m.file_id) || !existing_file_ids.contains(&m.file_id)
        })
        .count();
    let unmatched_shows = lib
        .shows
        .values()
        .filter(|show| {
            show.seasons
                .values()
                .flat_map(|s| s.episodes.iter())
                .all(|ep| !matched.tv.contains_key(&ep.file_id))
        })
        .count();
    let unmatched_total = unmatched_movies + unmatched_shows;

    let unmatched_title = if unmatched_total == 0 {
        String::new()
    } else {
        format!(
            "{unmatched_total} unmatched item{}",
            if unmatched_total == 1 { "" } else { "s" }
        )
    };
    let unmatched_detail = {
        let mut parts = Vec::new();
        if unmatched_movies > 0 {
            parts.push(format!(
                "{} movie{}",
                unmatched_movies,
                if unmatched_movies == 1 { "" } else { "s" }
            ));
        }
        if unmatched_shows > 0 {
            parts.push(format!(
                "{} show{}",
                unmatched_shows,
                if unmatched_shows == 1 { "" } else { "s" }
            ));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("{} still need metadata", parts.join(" and "))
        }
    };

    media_movies_model.set_vec(movies);
    media_shows_model.set_vec(shows);
    app.set_media_has_unmatched(unmatched_total > 0);
    app.set_media_unmatched_title(unmatched_title.as_str().into());
    app.set_media_unmatched_detail(unmatched_detail.as_str().into());

    missing_posters.sort_unstable();
    missing_posters.dedup();
    missing_posters
}

fn refresh_metadata_ui(
    app: &AppWindow,
    model: &Rc<VecModel<MetadataItem>>,
    state: &Rc<RefCell<MetadataUiState>>,
    tree: &Arc<RwLock<UnifiedDirectoryTree>>,
    matched_store: &Arc<MatchedStore>,
) {
    let rebuilt = {
        let tree = tree.read().unwrap();
        let matched = matched_store.get_matched_snapshot().unwrap_or_default();
        build_metadata_rows(&tree, &matched)
    };
    state.borrow_mut().rows = rebuilt;
    let query = app.get_metadata_query().to_string().to_lowercase();
    let filter = app.get_metadata_filter();
    let hide_matched = app.get_metadata_hide_matched();
    let state_ref = state.borrow();
    let mut items = Vec::new();
    let mut total = 0;
    let mut tv_count = 0;
    let mut movie_count = 0;
    let mut unmatched = 0;

    for row in &state_ref.rows {
        let is_show = matches!(row.kind, MetadataRowKind::Show);
        let is_movie = matches!(row.kind, MetadataRowKind::Movie);
        let is_episode = matches!(row.kind, MetadataRowKind::Episode);
        if is_episode && !state_ref.expanded.contains(&row.parent_id) {
            continue;
        }
        if filter == 1 && is_movie {
            continue;
        }
        if filter == 2 && (is_show || is_episode) {
            continue;
        }
        if hide_matched {
            if row.matched {
                continue;
            }
        }
        if !row_matches_query(row, &query) {
            continue;
        }
        if is_show {
            tv_count += 1;
        }
        if is_movie {
            movie_count += 1;
        }
        if !is_episode {
            total += 1;
            if !row.matched {
                unmatched += 1;
            }
        }
        let media_type = match row.kind {
            MetadataRowKind::Show => "TV",
            MetadataRowKind::Episode => "Episode",
            MetadataRowKind::Movie => "Movie",
        };
        items.push(MetadataItem {
            id: row.id,
            parent_id: row.parent_id,
            media_type: media_type.into(),
            title: row.title.as_str().into(),
            subtitle: row.subtitle.as_str().into(),
            badge: row.badge.as_str().into(),
            filename: row.filename.as_str().into(),
            relative_path: row.relative_path.as_str().into(),
            season: row.season,
            episode: row.episode,
            matched: row.matched,
            expanded: state_ref.expanded.contains(&row.id),
            selected: state_ref.selected.contains(&row.id),
        });
    }
    model.set_vec(items);
    app.set_metadata_total_count(total);
    app.set_metadata_tv_count(tv_count);
    app.set_metadata_movie_count(movie_count);
    app.set_metadata_unmatched_count(unmatched);
    app.set_metadata_selected_count(state_ref.selected.len() as i32);
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
    let matched_store = Arc::new(MatchedStore::load()?);
    let tmdb_store = Arc::new(TMDBStore::load()?);
    let tvmaze_store = Arc::new(TVMazeStore::load()?);
    let tmdb_api = Arc::new(metadata::TMDBAPI::new(config.clone(), tmdb_store.clone()));
    let tvmaze_api = Arc::new(metadata::TVMazeAPI::new(tvmaze_store.clone()));
    let metadata_api = Arc::new(metadata::MetadataAPI::new(
        matched_store.clone(),
        tmdb_api.clone(),
        tvmaze_api.clone(),
    ));
    let file_state = Arc::new(RwLock::new(FileStateStore::load()?));
    let client = PutioClient::new();
    let rt = Arc::new(Runtime::new()?);

    let app = AppWindow::new()?;
    app.set_files_mode(config.files_mode());
    app.set_files_sort(config.files_sort());
    app.set_files_sort_descending(config.files_sort_descending());
    app.set_view(VIEW_LOADING);
    app.set_loading_message("Checking sign-in…".into());

    let embedded_player = player::EmbeddedPlayer::install(
        &app,
        client.clone(),
        config.clone(),
        rt.clone(),
        VIEW_PLAYER,
        VIEW_FILES,
    );

    // Tree is shared across threads
    let tree: Arc<RwLock<UnifiedDirectoryTree>> =
        Arc::new(RwLock::new(UnifiedDirectoryTree::default()));
    let sync_profiles: Arc<RwLock<Vec<putio::sync::SyncProfile>>> =
        Arc::new(RwLock::new(Vec::new()));
    let files_refreshing = Arc::new(AtomicBool::new(false));
    let pending_local_clear: Rc<RefCell<Option<i32>>> = Rc::new(RefCell::new(None));
    // UI-thread-only state
    let current_folder: Rc<RefCell<u64>> = Rc::new(RefCell::new(0));
    let path_stack: Rc<RefCell<Vec<(u64, String)>>> =
        Rc::new(RefCell::new(vec![(0u64, "put.io".to_string())]));
    let oauth_flow: Rc<RefCell<OauthFlow>> = Rc::new(RefCell::new(OauthFlow::default()));

    let visible_model = Rc::new(VecModel::from(Vec::<FileItem>::new()));
    let path_model = Rc::new(VecModel::from(Vec::<PathSegment>::new()));
    let metadata_model = Rc::new(VecModel::from(Vec::<MetadataItem>::new()));
    let metadata_state = Rc::new(RefCell::new(MetadataUiState::new()));
    let media_movies_model = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
    let media_shows_model = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
    app.set_visible_items(ModelRc::from(visible_model.clone()));
    app.set_path_segments(ModelRc::from(path_model.clone()));
    app.set_metadata_items(ModelRc::from(metadata_model.clone()));
    app.set_media_movies(ModelRc::from(media_movies_model.clone()));
    app.set_media_shows(ModelRc::from(media_shows_model.clone()));
    let _ = metadata_item_empty();
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

    let metadata_refresh_now = {
        let weak = app.as_weak();
        let metadata_model = metadata_model.clone();
        let metadata_state = metadata_state.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                refresh_metadata_ui(
                    &app,
                    &metadata_model,
                    &metadata_state,
                    &tree,
                    &matched_store,
                );
            }
        }
    };

    let media_refresh_now = {
        let weak = app.as_weak();
        let media_movies_model = media_movies_model.clone();
        let media_shows_model = media_shows_model.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let file_state = file_state.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else { return };
            let missing = refresh_media_ui(
                &app,
                &media_movies_model,
                &media_shows_model,
                &tree,
                &matched_store,
                &tmdb_store,
                &file_state,
            );
            if !missing.is_empty() {
                let weak = weak.clone();
                rt.spawn(async move {
                    download_posters(missing).await;
                    let _ = weak.upgrade_in_event_loop(|app| {
                        app.invoke_media_refresh();
                    });
                });
            }
        }
    };

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
            let matched_path = matched_store.path();
            let tmdb_path = tmdb_store.path();
            let tvmaze_path = tvmaze_store.path();
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
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let tvmaze_store = tvmaze_store.clone();
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
                            app.invoke_metadata_criteria_changed();
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
                                        app.invoke_metadata_criteria_changed();
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
        let cfg = config.clone();
        let r = request_refresh_now.clone();
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
        let r = request_refresh_now.clone();
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
        let r = request_refresh_now.clone();
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
                            error!("write tree: {e}");
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
        let r = request_refresh_now.clone();
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

    app.on_files_menu_action({
        let weak = app.as_weak();
        let tree = tree.clone();
        let current_folder = current_folder.clone();
        let embedded_player = embedded_player.clone();
        move |action, id| {
            info!("menu action: {action} on {id}");
            if action.as_str() != "play" && action.as_str() != "play-mpv" {
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

    app.on_metadata_toggle_item({
        let metadata_state = metadata_state.clone();
        let refresh = metadata_refresh_now.clone();
        move |id| {
            let mut state = metadata_state.borrow_mut();
            if !state.selected.insert(id) {
                state.selected.remove(&id);
            }
            drop(state);
            refresh();
        }
    });
    app.on_metadata_toggle_expand({
        let metadata_state = metadata_state.clone();
        let refresh = metadata_refresh_now.clone();
        move |id| {
            let mut state = metadata_state.borrow_mut();
            if !state.expanded.insert(id) {
                state.expanded.remove(&id);
            }
            drop(state);
            refresh();
        }
    });
    app.on_metadata_select_unmatched({
        let metadata_state = metadata_state.clone();
        let refresh = metadata_refresh_now.clone();
        move || {
            let mut state = metadata_state.borrow_mut();
            let ids = state
                .rows
                .iter()
                .filter(|row| !matches!(row.kind, MetadataRowKind::Episode))
                .map(|row| row.id)
                .collect::<Vec<_>>();
            state.selected.extend(ids);
            drop(state);
            refresh();
        }
    });
    app.on_metadata_clear_selection({
        let metadata_state = metadata_state.clone();
        let refresh = metadata_refresh_now.clone();
        move || {
            metadata_state.borrow_mut().selected.clear();
            refresh();
        }
    });
    app.on_metadata_fetch({
        let weak = app.as_weak();
        let metadata_state = metadata_state.clone();
        let metadata_api = metadata_api.clone();
        let tmdb_api = tmdb_api.clone();
        let tvmaze_api = tvmaze_api.clone();
        let rt = rt.clone();
        move || {
            let candidates = {
                let state = metadata_state.borrow();
                metadata_fetch_candidates(&state)
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            if candidates.is_empty() {
                app.set_metadata_status("Select movies or TV shows to fetch metadata.".into());
                return;
            }
            app.set_metadata_busy(true);
            app.set_metadata_status(format!("Fetching metadata for {} selected item{}...", candidates.len(), if candidates.len() == 1 { "" } else { "s" }).into());

            let weak = weak.clone();
            let metadata_api = metadata_api.clone();
            let tmdb_api = tmdb_api.clone();
            let tvmaze_api = tvmaze_api.clone();
            rt.spawn(async move {
                let total = candidates.len();
                let mut matched_movies = 0usize;
                let mut matched_episodes = 0usize;
                let mut misses = 0usize;
                let mut errors = Vec::<String>::new();

                for candidate in candidates {
                    match candidate {
                        MetadataFetchCandidate::Movie { file_id, title, year } => {
                            let query = if year > 0 {
                                format!("{title} {year}")
                            } else {
                                title.clone()
                            };
                            match tmdb_api.search_movie(&query, 1).await {
                                Ok(results) => {
                                    if let Some(result) = results.first() {
                                        let _ = metadata_api.seed_movies(&[result.id]).await;
                                        let item = MatchItemByFileID {
                                            file_id,
                                            kind: "movie".to_string(),
                                            tmdb_id: result.id,
                                            source: "tmdb".to_string(),
                                        };
                                        match metadata_api.bulk_store_matches_by_file_id(&[item]) {
                                            Ok(()) => matched_movies += 1,
                                            Err(e) => errors.push(format!("{title}: {e}")),
                                        }
                                    } else {
                                        misses += 1;
                                    }
                                }
                                Err(e) => errors.push(format!("{title}: {e}")),
                            }
                        }
                        MetadataFetchCandidate::Show { title, episodes } => {
                            let outcome = match_show_metadata(
                                &title,
                                &episodes,
                                &metadata_api,
                                &tmdb_api,
                                &tvmaze_api,
                            )
                            .await;
                            matched_episodes += outcome.matched_episodes;
                            if outcome.missed {
                                misses += 1;
                            }
                            errors.extend(outcome.errors);
                        }
                    }
                }

                let mut message = format!(
                    "Fetched {total} item{}: matched {matched_movies} movie{} and {matched_episodes} episode{}.",
                    if total == 1 { "" } else { "s" },
                    if matched_movies == 1 { "" } else { "s" },
                    if matched_episodes == 1 { "" } else { "s" }
                );
                if misses > 0 {
                    message.push_str(&format!(" {misses} had no automatic match."));
                }
                if !errors.is_empty() {
                    message.push_str(&format!(" {} error{}.", errors.len(), if errors.len() == 1 { "" } else { "s" }));
                    if let Some(first) = errors.first() {
                        message.push_str(&format!(" First: {first}"));
                    }
                }

                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_metadata_busy(false);
                    app.set_metadata_status(message.into());
                    app.invoke_metadata_clear_selection();
                    app.invoke_metadata_criteria_changed();
                    app.invoke_settings_refresh();
                    app.invoke_media_refresh();
                });
            });
        }
    });
    app.on_metadata_criteria_changed({
        let refresh = metadata_refresh_now.clone();
        move || refresh()
    });

    app.on_media_refresh({
        let refresh = media_refresh_now.clone();
        move || refresh()
    });

    app.on_media_item_clicked({
        let weak = app.as_weak();
        move |file_id| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if file_id.is_empty() {
                app.set_view(VIEW_FILES);
                return;
            }
            if let Ok(id) = file_id.parse::<u64>() {
                app.invoke_files_menu_action("play".into(), truncate_id(id));
            }
        }
    });

    app.on_media_scrape_unmatched({
        let weak = app.as_weak();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let metadata_api = metadata_api.clone();
        let tmdb_api = tmdb_api.clone();
        let tvmaze_api = tvmaze_api.clone();
        let rt = rt.clone();
        move || {
            let candidates = {
                let tree_guard = tree.read().unwrap();
                let matched = matched_store.get_matched_snapshot().unwrap_or_default();
                build_unmatched_candidates_from_tree(&tree_guard, &matched)
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            if candidates.is_empty() {
                return;
            }
            app.set_media_show_error_flash(false);
            app.set_media_show_success_flash(false);
            let weak = weak.clone();
            let metadata_api = metadata_api.clone();
            let tmdb_api = tmdb_api.clone();
            let tvmaze_api = tvmaze_api.clone();
            rt.spawn(async move {
                let mut matched_movies = 0usize;
                let mut matched_episodes = 0usize;
                let mut misses = 0usize;
                let mut errors = Vec::<String>::new();
                for candidate in candidates {
                    match candidate {
                        MetadataFetchCandidate::Movie {
                            file_id,
                            title,
                            year,
                        } => {
                            let query = if year > 0 {
                                format!("{title} {year}")
                            } else {
                                title.clone()
                            };
                            match tmdb_api.search_movie(&query, 1).await {
                                Ok(results) => {
                                    if let Some(result) = results.first() {
                                        let _ = metadata_api.seed_movies(&[result.id]).await;
                                        let item = MatchItemByFileID {
                                            file_id,
                                            kind: "movie".to_string(),
                                            tmdb_id: result.id,
                                            source: "tmdb".to_string(),
                                        };
                                        match metadata_api.bulk_store_matches_by_file_id(&[item]) {
                                            Ok(()) => matched_movies += 1,
                                            Err(e) => errors.push(format!("{title}: {e}")),
                                        }
                                    } else {
                                        misses += 1;
                                    }
                                }
                                Err(e) => errors.push(format!("{title}: {e}")),
                            }
                        }
                        MetadataFetchCandidate::Show { title, episodes } => {
                            let outcome = match_show_metadata(
                                &title,
                                &episodes,
                                &metadata_api,
                                &tmdb_api,
                                &tvmaze_api,
                            )
                            .await;
                            matched_episodes += outcome.matched_episodes;
                            if outcome.missed {
                                misses += 1;
                            }
                            errors.extend(outcome.errors);
                        }
                    }
                }
                let success_text = format!(
                    "Matched {} movie{} and {} episode{}{}.",
                    matched_movies,
                    if matched_movies == 1 { "" } else { "s" },
                    matched_episodes,
                    if matched_episodes == 1 { "" } else { "s" },
                    if misses > 0 {
                        format!(" ({misses} unresolved)")
                    } else {
                        String::new()
                    }
                );
                let error_text = errors.first().map(|e| e.clone()).unwrap_or_default();
                let had_success = matched_movies > 0 || matched_episodes > 0;
                let had_errors = !errors.is_empty();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if had_success {
                        app.set_media_show_success_flash(true);
                        app.set_media_success_flash_text(success_text.as_str().into());
                    }
                    if had_errors {
                        app.set_media_show_error_flash(true);
                        app.set_media_error_flash_text(error_text.as_str().into());
                    }
                    app.invoke_settings_refresh();
                    app.invoke_media_refresh();
                });
            });
        }
    });

    app.on_media_dismiss_error({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_media_show_error_flash(false);
            }
        }
    });

    app.on_media_dismiss_success({
        let weak = app.as_weak();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_media_show_success_flash(false);
            }
        }
    });

    app.run()?;
    drop(rt);
    Ok(())
}
