//! Media library page: poster cache, movie/show grids, and Slint callbacks.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use slint::{ComponentHandle, Model, VecModel};
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio::runtime::Runtime;
use tracing::warn;

use crate::fileparser;
use crate::metadata::tmdb::{MovieDetails, TVSeasonDetails, TVSeriesDetails};
use crate::player::PlaybackQueueItem;
use crate::putio::types::DirectoryNode;
use crate::storage::file_state::{FileStateEntry, FileStateStore};
use crate::storage::matched_store::MatchedStore;
use crate::storage::tmdb_store::TMDBStore;
use crate::{AppWindow, MediaHeroBadge, MediaHeroItem, MediaItem, MediaResumeItem};

use super::metadata_ui::{build_unmatched_candidates_from_tree, fetch_metadata_candidates};
use super::models::UiModels;
use super::state::UiState;
use super::toast::{self, ToastKind};
use super::util::{format_runtime, make_initials};
use super::{Services, VIEW_FILES, VIEW_TV_SHOW};

pub(crate) struct MediaModelRefs<'a> {
    pub(crate) movies: &'a Rc<VecModel<MediaItem>>,
    pub(crate) shows: &'a Rc<VecModel<MediaItem>>,
    pub(crate) resume: &'a Rc<VecModel<MediaResumeItem>>,
    pub(crate) hero_badges: &'a Rc<VecModel<MediaHeroBadge>>,
}

pub(crate) struct MediaCacheRefs<'a> {
    pub(crate) movies: &'a Rc<RefCell<Vec<MediaItem>>>,
    pub(crate) shows: &'a Rc<RefCell<Vec<MediaItem>>>,
    pub(crate) resume: &'a Rc<RefCell<Vec<MediaResumeItem>>>,
    pub(crate) file_state: &'a Rc<RefCell<BTreeMap<String, FileStateEntry>>>,
    pub(crate) tv_episode_ids: &'a Rc<RefCell<HashMap<String, Vec<String>>>>,
}

pub(crate) fn collect_tree_file_ids(node: &DirectoryNode, ids: &mut HashSet<String>) {
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

fn remaining_label(entry: &crate::storage::file_state::FileStateEntry) -> String {
    if entry.duration_secs <= 0.0 || entry.position_secs >= entry.duration_secs {
        return format!(
            "{}% watched",
            (entry.progress_ratio() * 100.0).round() as i32
        );
    }
    let remaining = (entry.duration_secs - entry.position_secs).max(0.0).round() as i64;
    let minutes = ((remaining + 59) / 60).max(1);
    if minutes >= 60 {
        let h = minutes / 60;
        let m = minutes % 60;
        if m == 0 {
            format!("{h}h left")
        } else {
            format!("{h}h {m}m left")
        }
    } else {
        format!("{minutes} min left")
    }
}

fn genre_line(names: &[crate::metadata::tmdb::Genre]) -> String {
    names
        .iter()
        .filter_map(|g| {
            let name = g.name.trim();
            (!name.is_empty()).then_some(name)
        })
        .take(4)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn hero_badges_from_item(item: &MediaItem) -> Vec<MediaHeroBadge> {
    item.genre_line
        .split(" · ")
        .filter(|s| !s.trim().is_empty())
        .take(4)
        .map(|s| MediaHeroBadge { text: s.into() })
        .collect()
}

fn hero_from_item(item: &MediaItem) -> MediaHeroItem {
    let resume_label = if item.progress > 0.0 && item.progress < 0.9 {
        format!(
            "Resume · {}% watched",
            (item.progress * 100.0).round() as i32
        )
    } else {
        "Play".to_string()
    };
    MediaHeroItem {
        title: item.title.clone(),
        rating: item.rating.clone(),
        years: item.year_label.clone(),
        stats_line: item.stats_label.clone(),
        overview: item.overview.clone(),
        resume_label: resume_label.as_str().into(),
        poster: item.poster.clone(),
        backdrop: item.backdrop.clone(),
        progress: item.progress,
        is_tv: item.is_tv,
        initials: item.initials.clone(),
        file_id: item.file_id.clone(),
    }
}

fn collect_file_created_at(node: &DirectoryNode, map: &mut HashMap<String, String>) {
    for file in &node.files {
        map.insert(
            file.id.to_string(),
            file.created_at.clone().unwrap_or_default(),
        );
    }
    for child in &node.children {
        collect_file_created_at(child, map);
    }
}

fn max_created_at(file_ids: &[String], created_at_map: &HashMap<String, String>) -> String {
    file_ids
        .iter()
        .filter_map(|id| created_at_map.get(id))
        .max()
        .cloned()
        .unwrap_or_default()
}

fn movie_is_watched(file_id: &str, entries: &BTreeMap<String, FileStateEntry>) -> bool {
    entries
        .get(file_id)
        .map(FileStateEntry::is_completed)
        .unwrap_or(false)
}

fn series_is_watched(
    episode_file_ids: &[String],
    entries: &BTreeMap<String, FileStateEntry>,
) -> bool {
    !episode_file_ids.is_empty()
        && episode_file_ids.iter().all(|id| {
            entries
                .get(id)
                .map(FileStateEntry::is_completed)
                .unwrap_or(false)
        })
}

fn media_item_is_watched(
    item: &MediaItem,
    entries: &BTreeMap<String, FileStateEntry>,
    tv_episode_ids: &HashMap<String, Vec<String>>,
) -> bool {
    if item.is_tv {
        tv_episode_ids
            .get(item.file_id.as_str())
            .map(|ids| series_is_watched(ids, entries))
            .unwrap_or(false)
    } else {
        movie_is_watched(item.file_id.as_str(), entries)
    }
}

fn sort_media_items(items: &mut [MediaItem], sort_index: i32) {
    match sort_index {
        1 => items.sort_by(|a, b| b.added_at.cmp(&a.added_at)),
        _ => {
            #[allow(clippy::unnecessary_sort_by)]
            items.sort_by(|a, b| a.title.cmp(&b.title));
        }
    }
}

fn media_matches(
    item: &MediaItem,
    query: &str,
    filter_index: i32,
    entries: &BTreeMap<String, FileStateEntry>,
    tv_episode_ids: &HashMap<String, Vec<String>>,
) -> bool {
    if filter_index == 1 && item.is_tv {
        return false;
    }
    if filter_index == 2 && !item.is_tv {
        return false;
    }
    if filter_index == 3 && media_item_is_watched(item, entries, tv_episode_ids) {
        return false;
    }
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {}",
        item.title, item.meta, item.overview, item.genre_line
    )
    .to_lowercase();
    haystack.contains(query)
}

fn resume_matches(
    item: &MediaResumeItem,
    query: &str,
    filter_index: i32,
    entries: &BTreeMap<String, FileStateEntry>,
    tv_episode_ids: &HashMap<String, Vec<String>>,
) -> bool {
    if filter_index == 1 && item.kind != "MOVIE" {
        return false;
    }
    if filter_index == 2 && item.kind != "TV" {
        return false;
    }
    if filter_index == 3 {
        let watched = if item.kind == "TV" {
            tv_episode_ids
                .get(item.file_id.as_str())
                .map(|ids| series_is_watched(ids, entries))
                .unwrap_or(false)
        } else {
            movie_is_watched(item.file_id.as_str(), entries)
        };
        if watched {
            return false;
        }
    }
    if query.is_empty() {
        return true;
    }
    let haystack = format!("{} {}", item.title, item.detail).to_lowercase();
    haystack.contains(query)
}

fn apply_media_filter(app: &AppWindow, models: &MediaModelRefs<'_>, cache: &MediaCacheRefs<'_>) {
    let query = app.get_media_query().trim().to_lowercase();
    let filter_index = app.get_media_filter_index();
    let sort_index = app.get_media_sort_index();
    let entries = cache.file_state.borrow();
    let tv_episode_ids = cache.tv_episode_ids.borrow();

    let mut movies: Vec<MediaItem> = cache
        .movies
        .borrow()
        .iter()
        .filter(|item| media_matches(item, &query, filter_index, &entries, &tv_episode_ids))
        .cloned()
        .collect();
    let mut shows: Vec<MediaItem> = cache
        .shows
        .borrow()
        .iter()
        .filter(|item| media_matches(item, &query, filter_index, &entries, &tv_episode_ids))
        .cloned()
        .collect();
    let resume: Vec<MediaResumeItem> = cache
        .resume
        .borrow()
        .iter()
        .filter(|item| resume_matches(item, &query, filter_index, &entries, &tv_episode_ids))
        .cloned()
        .collect();

    sort_media_items(&mut movies, sort_index);
    sort_media_items(&mut shows, sort_index);

    let hero_source = resume
        .first()
        .and_then(|r| {
            movies
                .iter()
                .chain(shows.iter())
                .find(|item| item.file_id == r.file_id)
        })
        .or_else(|| shows.first())
        .or_else(|| movies.first());

    if let Some(item) = hero_source {
        app.set_media_hero(hero_from_item(item));
        models.hero_badges.set_vec(hero_badges_from_item(item));
    } else {
        app.set_media_hero(MediaHeroItem::default());
        models.hero_badges.set_vec(Vec::new());
    }

    models.movies.set_vec(movies);
    models.shows.set_vec(shows);
    models.resume.set_vec(resume);
}

pub(crate) fn refresh_media_ui(
    app: &AppWindow,
    models: &MediaModelRefs<'_>,
    cache: &MediaCacheRefs<'_>,
    tree: &Arc<RwLock<crate::putio::types::UnifiedDirectoryTree>>,
    matched_store: &Arc<MatchedStore>,
    tmdb_store: &Arc<TMDBStore>,
    file_state: &Arc<RwLock<FileStateStore>>,
) -> Vec<String> {
    let matched = matched_store.get_matched_snapshot().unwrap_or_default();
    let tmdb_cache = tmdb_store.get_cache_snapshot().unwrap_or_default();
    let file_state_entries = file_state.read().unwrap().entries().clone();

    let mut existing_file_ids = HashSet::<String>::new();
    let mut created_at_map = HashMap::<String, String>::new();
    {
        let tree_guard = tree.read().unwrap();
        collect_tree_file_ids(&tree_guard.root, &mut existing_file_ids);
        collect_file_created_at(&tree_guard.root, &mut created_at_map);
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
    let mut episode_display: HashMap<i32, (i32, i32, String)> = HashMap::new();
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
                                episode_display.insert(
                                    ep.id,
                                    (season.season_number, ep.episode_number, ep.name.clone()),
                                );
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
                (true, false) => rt.clone(),
                (false, true) => year.clone(),
                (false, false) => format!("{year} · {rt}"),
            };
            let rating = if d.vote_average > 0.0 {
                format!("{:.1}", d.vote_average)
            } else {
                String::new()
            };
            let progress = file_state_entries
                .get(&file_id)
                .map(|e| e.progress_ratio())
                .unwrap_or(0.0);
            let poster = if d.poster_path.is_empty() {
                Default::default()
            } else if let Some(img) = load_cached_poster(&d.poster_path) {
                img
            } else {
                missing_posters.push(d.poster_path.clone());
                Default::default()
            };
            let backdrop = if d.backdrop_path.is_empty() {
                Default::default()
            } else if let Some(img) = load_cached_backdrop(&d.backdrop_path) {
                img
            } else {
                missing_posters.push(d.backdrop_path.clone());
                Default::default()
            };
            let genres = genre_line(&d.genres);
            let added_at = created_at_map.get(&file_id).cloned().unwrap_or_default();
            MediaItem {
                title: d.title.as_str().into(),
                meta: meta.as_str().into(),
                rating: rating.as_str().into(),
                poster,
                backdrop,
                overview: d.overview.as_str().into(),
                year_label: year.as_str().into(),
                stats_label: rt.as_str().into(),
                genre_line: genres.as_str().into(),
                resolution: "".into(),
                progress,
                is_tv: false,
                initials: make_initials(&d.title),
                file_id: file_id.as_str().into(),
                added_at: added_at.as_str().into(),
            }
        })
        .collect();
    // `sort_by_key` would need an owned key here; compare SharedString values directly.
    #[allow(clippy::unnecessary_sort_by)]
    movies.sort_by(|a, b| a.title.cmp(&b.title));

    let mut show_groups: std::collections::BTreeMap<
        i32,
        (
            TVSeriesDetails,
            std::collections::BTreeSet<i32>,
            usize,
            Vec<String>,
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
                Vec::new(),
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
        entry.3.push(file_id.clone());
    }

    let mut tv_episode_ids = HashMap::<String, Vec<String>>::new();
    let mut shows: Vec<MediaItem> = show_groups
        .into_iter()
        .map(|(series_id, (d, seasons, ep_count, file_ids))| {
            tv_episode_ids.insert(format!("tv:{series_id}"), file_ids.clone());
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
            let backdrop = if d.backdrop_path.is_empty() {
                Default::default()
            } else if let Some(img) = load_cached_backdrop(&d.backdrop_path) {
                img
            } else {
                missing_posters.push(d.backdrop_path.clone());
                Default::default()
            };
            let progress_total: f32 = file_ids
                .iter()
                .map(|id| {
                    file_state_entries
                        .get(id)
                        .map(|entry| entry.progress_ratio())
                        .unwrap_or(0.0)
                })
                .sum();
            let progress = if file_ids.is_empty() {
                0.0
            } else {
                (progress_total / file_ids.len() as f32).clamp(0.0, 1.0)
            };
            let genres = genre_line(&d.genres);
            let stats_label = format!(
                "{season_count} season{} · {ep_count} episode{}",
                if season_count == 1 { "" } else { "s" },
                if ep_count == 1 { "" } else { "s" }
            );
            let show_file_id = format!("tv:{series_id}");
            let added_at = max_created_at(&file_ids, &created_at_map);
            MediaItem {
                title: d.name.as_str().into(),
                meta: meta.as_str().into(),
                rating: rating.as_str().into(),
                poster,
                backdrop,
                overview: d.overview.as_str().into(),
                year_label: year.as_str().into(),
                stats_label: stats_label.as_str().into(),
                genre_line: genres.as_str().into(),
                resolution: "".into(),
                progress,
                is_tv: true,
                initials: make_initials(&d.name),
                file_id: show_file_id.as_str().into(),
                added_at: added_at.as_str().into(),
            }
        })
        .collect();
    // `sort_by_key` would need an owned key here; compare SharedString values directly.
    #[allow(clippy::unnecessary_sort_by)]
    shows.sort_by(|a, b| a.title.cmp(&b.title));

    let mut resume_rows: Vec<(i64, MediaResumeItem)> = Vec::new();
    for movie in &movies {
        let Some(entry) = file_state_entries.get(movie.file_id.as_str()) else {
            continue;
        };
        let progress = entry.progress_ratio();
        if !(0.05..0.9).contains(&progress) {
            continue;
        }
        resume_rows.push((
            entry.updated_at,
            MediaResumeItem {
                title: movie.title.clone(),
                kind: "MOVIE".into(),
                detail: "Movie".into(),
                left_label: remaining_label(entry).as_str().into(),
                poster: movie.poster.clone(),
                initials: movie.initials.clone(),
                progress,
                file_id: movie.file_id.clone(),
            },
        ));
    }

    for (file_id, &episode_id) in &matched.tv {
        if !existing_file_ids.contains(file_id) {
            continue;
        }
        let Some(entry) = file_state_entries.get(file_id) else {
            continue;
        };
        let progress = entry.progress_ratio();
        if !(0.05..0.9).contains(&progress) {
            continue;
        }
        let Some(series_id) = episode_to_series.get(&episode_id) else {
            continue;
        };
        let series_file_id = format!("tv:{series_id}");
        let Some(show) = shows.iter().find(|item| item.file_id == series_file_id) else {
            continue;
        };
        let detail = episode_display
            .get(&episode_id)
            .map(|(season, episode, name)| {
                if name.trim().is_empty() {
                    format!("S{season} · E{episode}")
                } else {
                    format!("S{season} · E{episode} - {name}")
                }
            })
            .unwrap_or_else(|| "Episode".to_string());
        resume_rows.push((
            entry.updated_at,
            MediaResumeItem {
                title: show.title.clone(),
                kind: "TV".into(),
                detail: detail.as_str().into(),
                left_label: remaining_label(entry).as_str().into(),
                poster: show.poster.clone(),
                initials: show.initials.clone(),
                progress,
                file_id: show.file_id.clone(),
            },
        ));
    }
    resume_rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let resume_items: Vec<MediaResumeItem> = resume_rows
        .into_iter()
        .map(|(_, row)| row)
        .take(10)
        .collect();

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

    *cache.movies.borrow_mut() = movies;
    *cache.shows.borrow_mut() = shows;
    *cache.resume.borrow_mut() = resume_items;
    *cache.file_state.borrow_mut() = file_state_entries;
    *cache.tv_episode_ids.borrow_mut() = tv_episode_ids;
    apply_media_filter(app, models, cache);
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
    embedded_player: &crate::player::EmbeddedPlayer,
) {
    let weak = app.as_weak();
    let tree = state.tree.clone();
    let matched_store = services.matched_store.clone();
    let tmdb_store = services.tmdb_store.clone();
    let metadata_api = services.metadata_api.clone();
    let tmdb_api = services.tmdb_api.clone();
    let tvmaze_api = services.tvmaze_api.clone();
    let embedded_player = embedded_player.clone();
    let watch_sync = services.watch_sync.clone();

    app.on_media_refresh({
        let refresh = media_refresh.clone();
        move || refresh()
    });

    app.on_media_filter_changed({
        let weak = weak.clone();
        let media_movies_model = models.media_movies.clone();
        let media_shows_model = models.media_shows.clone();
        let media_resume_model = models.media_resume.clone();
        let media_hero_badges_model = models.media_hero_badges.clone();
        let all_movies = state.media_all_movies.clone();
        let all_shows = state.media_all_shows.clone();
        let all_resume = state.media_all_resume.clone();
        let media_file_state = state.media_file_state.clone();
        let media_tv_episode_ids = state.media_tv_episode_ids.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                let media_models = MediaModelRefs {
                    movies: &media_movies_model,
                    shows: &media_shows_model,
                    resume: &media_resume_model,
                    hero_badges: &media_hero_badges_model,
                };
                let media_cache = MediaCacheRefs {
                    movies: &all_movies,
                    shows: &all_shows,
                    resume: &all_resume,
                    file_state: &media_file_state,
                    tv_episode_ids: &media_tv_episode_ids,
                };
                apply_media_filter(&app, &media_models, &media_cache);
            }
        }
    });

    app.on_media_sort_changed({
        let weak = weak.clone();
        let media_movies_model = models.media_movies.clone();
        let media_shows_model = models.media_shows.clone();
        let media_resume_model = models.media_resume.clone();
        let media_hero_badges_model = models.media_hero_badges.clone();
        let all_movies = state.media_all_movies.clone();
        let all_shows = state.media_all_shows.clone();
        let all_resume = state.media_all_resume.clone();
        let media_file_state = state.media_file_state.clone();
        let media_tv_episode_ids = state.media_tv_episode_ids.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                let media_models = MediaModelRefs {
                    movies: &media_movies_model,
                    shows: &media_shows_model,
                    resume: &media_resume_model,
                    hero_badges: &media_hero_badges_model,
                };
                let media_cache = MediaCacheRefs {
                    movies: &all_movies,
                    shows: &all_shows,
                    resume: &all_resume,
                    file_state: &media_file_state,
                    tv_episode_ids: &media_tv_episode_ids,
                };
                apply_media_filter(&app, &media_models, &media_cache);
            }
        }
    });

    app.on_media_item_clicked({
        let weak = weak.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let file_state = services.file_state.clone();
        let tv_show_seasons_model = models.tv_seasons.clone();
        let tv_show_episodes_model = models.tv_episodes.clone();
        let tv_show_hero_badges_model = models.tv_hero_badges.clone();
        let tv_show_detail_items_model = models.tv_detail_items.clone();
        let media_movies_model = models.media_movies.clone();
        let embedded_player = embedded_player.clone();
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
                        &file_state,
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
                let movie = (0..media_movies_model.row_count())
                    .filter_map(|idx| media_movies_model.row_data(idx))
                    .find(|item| item.file_id == file_id)
                    .map(|item| PlaybackQueueItem {
                        file_id: id,
                        title: item.title.to_string(),
                        meta: item.meta.to_string(),
                    })
                    .unwrap_or_else(|| PlaybackQueueItem {
                        file_id: id,
                        title: file_id.to_string(),
                        meta: String::new(),
                    });
                embedded_player.play_queue(&app, vec![movie], id);
            }
        }
    });

    app.on_media_watch_toggle({
        let weak = weak.clone();
        let file_state = services.file_state.clone();
        let watch_sync = watch_sync.clone();
        let media_refresh = media_refresh.clone();
        move |file_id| {
            let Ok(id) = file_id.as_str().parse::<u64>() else {
                return;
            };
            let currently_watched = file_state
                .read()
                .unwrap()
                .entries()
                .get(&id.to_string())
                .map(|entry| entry.is_completed())
                .unwrap_or(false);
            watch_sync.mark_watched(id, !currently_watched);
            media_refresh();
            if let Some(app) = weak.upgrade() {
                app.invoke_request_refresh();
                app.invoke_settings_refresh();
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
                        toast::show(
                            &app,
                            ToastKind::Success,
                            "Metadata matches saved",
                            success_text.as_str(),
                        );
                    }
                    if had_errors {
                        app.set_media_show_error_flash(true);
                        app.set_media_error_flash_text(error_text.as_str().into());
                        toast::show(
                            &app,
                            ToastKind::Error,
                            "Could not scrape unmatched media",
                            error_text.as_str(),
                        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::file_state::FileStateEntry;

    fn sample_movie(file_id: &str, progress: f32) -> MediaItem {
        MediaItem {
            title: "Test Movie".into(),
            meta: "".into(),
            rating: "".into(),
            poster: Default::default(),
            backdrop: Default::default(),
            overview: "".into(),
            year_label: "".into(),
            stats_label: "".into(),
            genre_line: "".into(),
            resolution: "".into(),
            progress,
            is_tv: false,
            initials: "TM".into(),
            file_id: file_id.into(),
            added_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    fn sample_show(file_id: &str) -> MediaItem {
        MediaItem {
            title: "Test Show".into(),
            meta: "".into(),
            rating: "".into(),
            poster: Default::default(),
            backdrop: Default::default(),
            overview: "".into(),
            year_label: "".into(),
            stats_label: "".into(),
            genre_line: "".into(),
            resolution: "".into(),
            progress: 0.5,
            is_tv: true,
            initials: "TS".into(),
            file_id: file_id.into(),
            added_at: "2024-06-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn movie_is_watched_when_played_flag_set() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "42".to_string(),
            FileStateEntry {
                played: true,
                ..Default::default()
            },
        );
        assert!(movie_is_watched("42", &entries));
        assert!(!movie_is_watched("99", &entries));
    }

    #[test]
    fn series_is_watched_only_when_all_episodes_complete() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "1".to_string(),
            FileStateEntry {
                played: true,
                ..Default::default()
            },
        );
        entries.insert("2".to_string(), FileStateEntry::default());
        let ids = vec!["1".to_string(), "2".to_string()];
        assert!(!series_is_watched(&ids, &entries));
        entries.insert(
            "2".to_string(),
            FileStateEntry {
                played: true,
                ..Default::default()
            },
        );
        assert!(series_is_watched(&ids, &entries));
    }

    #[test]
    fn media_matches_unwatched_excludes_completed_movie() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "42".to_string(),
            FileStateEntry {
                played: true,
                ..Default::default()
            },
        );
        let item = sample_movie("42", 1.0);
        assert!(!media_matches(&item, "", 3, &entries, &HashMap::new()));
        let unwatched = sample_movie("99", 0.0);
        assert!(media_matches(&unwatched, "", 3, &entries, &HashMap::new()));
    }

    #[test]
    fn media_matches_type_filters() {
        let entries = BTreeMap::new();
        let tv = sample_show("tv:1");
        let movie = sample_movie("1", 0.0);
        assert!(!media_matches(&tv, "", 1, &entries, &HashMap::new()));
        assert!(media_matches(&movie, "", 1, &entries, &HashMap::new()));
        assert!(!media_matches(&movie, "", 2, &entries, &HashMap::new()));
        assert!(media_matches(&tv, "", 2, &entries, &HashMap::new()));
    }

    #[test]
    fn sort_media_items_by_added_at_newest_first() {
        let mut items = vec![
            sample_movie("1", 0.0),
            MediaItem {
                added_at: "2025-01-01T00:00:00Z".into(),
                ..sample_movie("2", 0.0)
            },
        ];
        sort_media_items(&mut items, 1);
        assert_eq!(items[0].file_id.as_str(), "2");
        assert_eq!(items[1].file_id.as_str(), "1");
    }
}
