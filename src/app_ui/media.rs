//! Media library page: poster cache, movie/show grids, and Slint callbacks.

use std::rc::Rc;
use std::sync::{Arc, RwLock};

use serde_json;
use slint::{ComponentHandle, VecModel};
use std::collections::HashMap;
use tokio::runtime::Runtime;
use tracing::warn;

use crate::fileparser;
use crate::metadata::tmdb::{MovieDetails, TVSeasonDetails, TVSeriesDetails};
use crate::putio::types::DirectoryNode;
use crate::storage::file_state::FileStateStore;
use crate::storage::matched_store::MatchedStore;
use crate::storage::tmdb_store::TMDBStore;
use crate::{AppWindow, MediaItem};

use super::metadata_ui::{build_unmatched_candidates_from_tree, fetch_metadata_candidates};
use super::models::UiModels;
use super::state::UiState;
use super::util::{format_runtime, make_initials, truncate_id};
use super::{Services, VIEW_FILES, VIEW_TV_SHOW};

pub(crate) fn collect_tree_file_ids(
    node: &DirectoryNode,
    ids: &mut std::collections::HashSet<String>,
) {
    for f in &node.files {
        ids.insert(f.id.to_string());
    }
    for child in &node.children {
        collect_tree_file_ids(child, ids);
    }
}

pub(crate) fn poster_cache_path(poster_path: &str) -> Option<std::path::PathBuf> {
    let filename = poster_path.trim_start_matches('/');
    if filename.is_empty() {
        return None;
    }
    Some(crate::storage::poster_cache_dir().ok()?.join(filename))
}

/// Cache path for w1280 images (stored in an `hd/` subdirectory).
fn poster_cache_path_hd(poster_path: &str) -> Option<std::path::PathBuf> {
    let filename = poster_path.trim_start_matches('/');
    if filename.is_empty() {
        return None;
    }
    Some(
        crate::storage::poster_cache_dir()
            .ok()?
            .join("hd")
            .join(filename),
    )
}

pub(crate) fn load_cached_poster(poster_path: &str) -> Option<slint::Image> {
    let path = poster_cache_path(poster_path)?;
    slint::Image::load_from_path(&path).ok()
}

/// Load a backdrop at w1280 quality; falls back to w342 if not yet cached.
pub(crate) fn load_cached_backdrop(poster_path: &str) -> Option<slint::Image> {
    if let Some(path) = poster_cache_path_hd(poster_path) {
        if let Ok(img) = slint::Image::load_from_path(&path) {
            return Some(img);
        }
    }
    load_cached_poster(poster_path)
}

pub(crate) async fn download_posters(poster_paths: Vec<String>) {
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

/// Download a backdrop at w1280 into the `hd/` cache subdirectory.
pub(crate) async fn download_backdrop_hd(poster_path: String) {
    let Some(cache_path) = poster_cache_path_hd(&poster_path) else {
        return;
    };
    if cache_path.exists() {
        return;
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("PutMPV/1.0")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let url = format!("https://image.tmdb.org/t/p/w1280{poster_path}");
    match client.get(&url).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(bytes) => {
                if let Some(parent) = cache_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&cache_path, &bytes) {
                    warn!(
                        "Failed to write HD backdrop cache {}: {e}",
                        cache_path.display()
                    );
                }
            }
            Err(e) => warn!("Failed to read HD backdrop bytes for {poster_path}: {e}"),
        },
        Err(e) => warn!("Failed to fetch HD backdrop {poster_path}: {e}"),
    }
}

pub(crate) fn refresh_media_ui(
    app: &AppWindow,
    media_movies_model: &Rc<VecModel<MediaItem>>,
    media_shows_model: &Rc<VecModel<MediaItem>>,
    tree: &Arc<RwLock<crate::putio::types::UnifiedDirectoryTree>>,
    matched_store: &Arc<MatchedStore>,
    tmdb_store: &Arc<TMDBStore>,
    file_state: &Arc<RwLock<FileStateStore>>,
) -> Vec<String> {
    let matched = matched_store.get_matched_snapshot().unwrap_or_default();
    let tmdb_cache = tmdb_store.get_cache_snapshot().unwrap_or_default();
    let file_state_entries = file_state.read().unwrap().entries().clone();

    let mut existing_file_ids = std::collections::HashSet::<String>::new();
    {
        let tree_guard = tree.read().unwrap();
        collect_tree_file_ids(&tree_guard.root, &mut existing_file_ids);
    }

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

    let mut episode_to_series: HashMap<i32, i32> = HashMap::new();
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
        .into_iter()
        .map(|(series_id, (d, seasons, ep_count, _file_id))| {
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
                file_id: format!("tv:{series_id}").as_str().into(),
            }
        })
        .collect();
    shows.sort_by(|a, b| a.title.to_string().cmp(&b.title.to_string()));

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

pub(crate) fn install(
    app: &AppWindow,
    media_refresh: std::rc::Rc<dyn Fn()>,
    services: &Services,
    state: &UiState,
    models: &UiModels,
    rt: &Arc<Runtime>,
) {
    let weak = app.as_weak();
    let tree = state.tree.clone();
    let matched_store = services.matched_store.clone();
    let tmdb_store = services.tmdb_store.clone();
    let metadata_api = services.metadata_api.clone();
    let tmdb_api = services.tmdb_api.clone();
    let tvmaze_api = services.tvmaze_api.clone();

    app.on_media_refresh({
        let refresh = media_refresh.clone();
        move || refresh()
    });

    app.on_media_item_clicked({
        let weak = weak.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let tv_show_seasons_model = models.tv_seasons.clone();
        let tv_show_episodes_model = models.tv_episodes.clone();
        let tv_show_hero_badges_model = models.tv_hero_badges.clone();
        let tv_show_detail_items_model = models.tv_detail_items.clone();
        let rt = rt.clone();
        move |file_id| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if file_id.is_empty() {
                app.set_view(VIEW_FILES);
                return;
            }
            if let Some(rest) = file_id.strip_prefix("tv:") {
                if let Ok(sid) = rest.parse::<i32>() {
                    app.set_tv_show_series_id(sid);
                    super::tv_show::refresh_tv_show_ui(
                        &app,
                        sid,
                        None,
                        &tree,
                        &matched_store,
                        &tmdb_store,
                        &tv_show_seasons_model,
                        &tv_show_episodes_model,
                        &tv_show_hero_badges_model,
                        &tv_show_detail_items_model,
                        &rt,
                    );
                    app.set_view(VIEW_TV_SHOW);
                    return;
                }
            }
            if let Ok(id) = file_id.parse::<u64>() {
                app.invoke_files_menu_action("play".into(), truncate_id(id));
            }
        }
    });

    app.on_media_scrape_unmatched({
        let weak = weak.clone();
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
                let summary =
                    fetch_metadata_candidates(candidates, metadata_api, tmdb_api, tvmaze_api).await;
                let success_text = summary.media_success_text();
                let error_text = summary.errors.first().cloned().unwrap_or_default();
                let had_success = summary.had_success();
                let had_errors = !summary.errors.is_empty();
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
        let weak = weak.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_media_show_error_flash(false);
            }
        }
    });

    app.on_media_dismiss_success({
        let weak = weak.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_media_show_success_flash(false);
            }
        }
    });
}
