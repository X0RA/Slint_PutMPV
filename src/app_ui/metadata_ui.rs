//! Metadata table: row model, fetch orchestration, and Slint callbacks.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};

use slint::{ComponentHandle, VecModel};
use tokio::runtime::Runtime;

use crate::fileparser;
use crate::metadata;
use crate::metadata::api::{EpisodeRefByFileID, MatchItemByFileID};
use crate::putio::types::UnifiedDirectoryTree;
use crate::storage::matched_store::{MatchedData, MatchedStore};
use crate::{AppWindow, MetadataItem};

use super::media::collect_tree_file_ids;
use super::state::UiState;
use super::util::stable_i32_id;
use super::Services;

#[derive(Debug, Clone)]
pub(crate) struct MetadataUiState {
    pub rows: Vec<MetadataRow>,
    pub expanded: std::collections::BTreeSet<i32>,
    pub selected: std::collections::BTreeSet<i32>,
}

impl MetadataUiState {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            expanded: std::collections::BTreeSet::new(),
            selected: std::collections::BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MetadataRowKind {
    Show,
    Episode,
    Movie,
}

#[derive(Debug, Clone)]
pub(crate) struct MetadataRow {
    pub id: i32,
    pub parent_id: i32,
    pub kind: MetadataRowKind,
    pub file_id: String,
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    pub filename: String,
    pub relative_path: String,
    pub season: i32,
    pub episode: i32,
    pub year: i32,
    pub matched: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum MetadataFetchCandidate {
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
pub(crate) struct MetadataFetchSummary {
    pub total: usize,
    pub matched_movies: usize,
    pub matched_episodes: usize,
    pub misses: usize,
    pub errors: Vec<String>,
}

impl MetadataFetchSummary {
    pub(crate) fn metadata_status(&self) -> String {
        let mut message = format!(
            "Fetched {} item{}: matched {} movie{} and {} episode{}.",
            self.total,
            if self.total == 1 { "" } else { "s" },
            self.matched_movies,
            if self.matched_movies == 1 { "" } else { "s" },
            self.matched_episodes,
            if self.matched_episodes == 1 { "" } else { "s" }
        );
        if self.misses > 0 {
            message.push_str(&format!(" {} had no automatic match.", self.misses));
        }
        if !self.errors.is_empty() {
            message.push_str(&format!(
                " {} error{}.",
                self.errors.len(),
                if self.errors.len() == 1 { "" } else { "s" }
            ));
            if let Some(first) = self.errors.first() {
                message.push_str(&format!(" First: {first}"));
            }
        }
        message
    }

    pub(crate) fn media_success_text(&self) -> String {
        format!(
            "Matched {} movie{} and {} episode{}{}.",
            self.matched_movies,
            if self.matched_movies == 1 { "" } else { "s" },
            self.matched_episodes,
            if self.matched_episodes == 1 { "" } else { "s" },
            if self.misses > 0 {
                format!(" ({} unresolved)", self.misses)
            } else {
                String::new()
            }
        )
    }

    pub(crate) fn had_success(&self) -> bool {
        self.matched_movies > 0 || self.matched_episodes > 0
    }
}

#[derive(Debug, Default)]
pub(crate) struct ShowMetadataMatchOutcome {
    pub matched_episodes: usize,
    pub missed: bool,
    pub errors: Vec<String>,
}

fn build_metadata_rows(tree: &UnifiedDirectoryTree, matched: &MatchedData) -> Vec<MetadataRow> {
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

pub(crate) fn metadata_fetch_candidates(state: &MetadataUiState) -> Vec<MetadataFetchCandidate> {
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

pub(crate) fn build_unmatched_candidates_from_tree(
    tree: &UnifiedDirectoryTree,
    matched: &MatchedData,
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
        let unmatched_episodes: Vec<_> = show
            .seasons
            .values()
            .flat_map(|s| s.episodes.iter())
            .filter(|ep| existing_ids.contains(&ep.file_id))
            .filter(|ep| !matched.tv.contains_key(&ep.file_id))
            .collect();
        if !unmatched_episodes.is_empty() {
            let episodes = unmatched_episodes
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

pub(crate) async fn match_show_metadata(
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

pub(crate) async fn fetch_metadata_candidates(
    candidates: Vec<MetadataFetchCandidate>,
    metadata_api: Arc<metadata::MetadataAPI>,
    tmdb_api: Arc<metadata::TMDBAPI>,
    tvmaze_api: Arc<metadata::TVMazeAPI>,
) -> MetadataFetchSummary {
    let mut summary = MetadataFetchSummary {
        total: candidates.len(),
        ..MetadataFetchSummary::default()
    };

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
                                Ok(()) => summary.matched_movies += 1,
                                Err(e) => summary.errors.push(format!("{title}: {e}")),
                            }
                        } else {
                            summary.misses += 1;
                        }
                    }
                    Err(e) => summary.errors.push(format!("{title}: {e}")),
                }
            }
            MetadataFetchCandidate::Show { title, episodes } => {
                let outcome =
                    match_show_metadata(&title, &episodes, &metadata_api, &tmdb_api, &tvmaze_api)
                        .await;
                summary.matched_episodes += outcome.matched_episodes;
                if outcome.missed {
                    summary.misses += 1;
                }
                summary.errors.extend(outcome.errors);
            }
        }
    }

    summary
}

pub(crate) fn refresh_metadata_ui(
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

pub(crate) fn install(
    app: &AppWindow,
    metadata_refresh: std::rc::Rc<dyn Fn()>,
    services: &Services,
    state: &UiState,
    _models: &crate::app_ui::models::UiModels,
    rt: &Arc<Runtime>,
) {
    let weak = app.as_weak();
    let metadata_state = state.metadata_state.clone();
    let tree = state.tree.clone();
    let auto_metadata_attempted = state.auto_metadata_attempted.clone();
    let auto_metadata_fetching = state.auto_metadata_fetching.clone();
    let matched_store = services.matched_store.clone();
    let metadata_api = services.metadata_api.clone();
    let tmdb_api = services.tmdb_api.clone();
    let tvmaze_api = services.tvmaze_api.clone();

    app.on_metadata_toggle_item({
        let metadata_state = metadata_state.clone();
        let refresh = metadata_refresh.clone();
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
        let refresh = metadata_refresh.clone();
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
        let refresh = metadata_refresh.clone();
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
        let refresh = metadata_refresh.clone();
        move || {
            metadata_state.borrow_mut().selected.clear();
            refresh();
        }
    });
    app.on_metadata_fetch({
        let weak = weak.clone();
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
            app.set_metadata_status(
                format!(
                    "Fetching metadata for {} selected item{}...",
                    candidates.len(),
                    if candidates.len() == 1 { "" } else { "s" }
                )
                .into(),
            );

            let weak = weak.clone();
            let metadata_api = metadata_api.clone();
            let tmdb_api = tmdb_api.clone();
            let tvmaze_api = tvmaze_api.clone();
            rt.spawn(async move {
                let summary =
                    fetch_metadata_candidates(candidates, metadata_api, tmdb_api, tvmaze_api).await;
                let message = summary.metadata_status();

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
        let refresh = metadata_refresh.clone();
        move || refresh()
    });

    app.on_auto_metadata_fetch_after_refresh({
        let weak = weak.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let attempted = auto_metadata_attempted.clone();
        let fetching = auto_metadata_fetching.clone();
        let metadata_api = metadata_api.clone();
        let tmdb_api = tmdb_api.clone();
        let tvmaze_api = tvmaze_api.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if !app.get_auto_metadata_fetch_enabled() || app.get_metadata_busy() {
                return;
            }
            if fetching.swap(true, Ordering::Relaxed) {
                return;
            }

            let raw_candidates = {
                let tree_guard = tree.read().unwrap();
                let matched = matched_store.get_matched_snapshot().unwrap_or_default();
                build_unmatched_candidates_from_tree(&tree_guard, &matched)
            };

            let mut attempted_guard = attempted.borrow_mut();
            let mut candidates = Vec::new();
            for candidate in raw_candidates {
                match candidate {
                    MetadataFetchCandidate::Movie {
                        file_id,
                        title,
                        year,
                    } => {
                        if attempted_guard.insert(file_id.clone()) {
                            candidates.push(MetadataFetchCandidate::Movie {
                                file_id,
                                title,
                                year,
                            });
                        }
                    }
                    MetadataFetchCandidate::Show { title, episodes } => {
                        let fresh_episodes = episodes
                            .into_iter()
                            .filter(|ep| attempted_guard.insert(ep.file_id.clone()))
                            .collect::<Vec<_>>();
                        if !fresh_episodes.is_empty() {
                            candidates.push(MetadataFetchCandidate::Show {
                                title,
                                episodes: fresh_episodes,
                            });
                        }
                    }
                }
            }
            drop(attempted_guard);

            if candidates.is_empty() {
                fetching.store(false, Ordering::Relaxed);
                return;
            }

            app.set_metadata_status(
                format!(
                    "Automatically fetching metadata for {} unmatched item{}...",
                    candidates.len(),
                    if candidates.len() == 1 { "" } else { "s" }
                )
                .into(),
            );

            let weak = weak.clone();
            let fetching = fetching.clone();
            let metadata_api = metadata_api.clone();
            let tmdb_api = tmdb_api.clone();
            let tvmaze_api = tvmaze_api.clone();
            rt.spawn(async move {
                let summary =
                    fetch_metadata_candidates(candidates, metadata_api, tmdb_api, tvmaze_api).await;
                let message = format!(
                    "Automatic metadata fetch complete. {}",
                    summary.metadata_status()
                );
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_metadata_status(message.into());
                    app.invoke_metadata_criteria_changed();
                    app.invoke_settings_refresh();
                    app.invoke_media_refresh();
                });
                fetching.store(false, Ordering::Relaxed);
            });
        }
    });
}
