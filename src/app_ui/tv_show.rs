//! TV series detail page: season tabs, episode rows, hero, and Slint callbacks.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use crate::metadata::tmdb::{Episode, TVSeasonDetails, TVSeriesDetails};
use slint::{ComponentHandle, Model, VecModel};
use tokio::runtime::Runtime;

use crate::player::PlaybackQueueItem;
use crate::putio::types::UnifiedDirectoryTree;
use crate::storage::file_state::FileStateStore;
use crate::storage::matched_store::MatchedStore;
use crate::storage::tmdb_store::{CacheEntry, TMDBStore};
use crate::{AppWindow, TvDetailItem, TvEpisodeCredit, TvEpisodeRow, TvHeroBadge, TvSeasonTab};

use super::media::{
    collect_tree_file_ids, download_backdrop_hd, download_posters, load_cached_backdrop,
    load_cached_poster,
};
use super::models::UiModels;
use super::state::UiState;
use super::util::truncate_id;
use super::{Services, VIEW_MEDIA};

pub(crate) fn parse_season_cache_key(key: &str) -> Option<i32> {
    let rest = key.strip_prefix("season_")?;
    rest.split('_').next()?.parse().ok()
}

pub(crate) fn load_tv_season_details_from_sub(
    sub: &std::collections::HashMap<String, CacheEntry>,
    season_number: i32,
) -> Option<TVSeasonDetails> {
    for (key, entry) in sub {
        if parse_season_cache_key(key) == Some(season_number) {
            return serde_json::from_value(entry.data.clone()).ok();
        }
    }
    None
}

fn tv_status_badge_label(status: &str) -> Option<String> {
    if status.is_empty() {
        return None;
    }
    Some(
        match status {
            "Ended" => "Ended",
            "Canceled" | "Cancelled" => "Canceled",
            "Returning Series" => "Returning",
            "In Production" => "In production",
            "To Be Determined" => "TBD",
            "Pilot" => "Pilot",
            "Planned" => "Planned",
            other => other,
        }
        .to_string(),
    )
}

fn tv_status_detail_label(status: &str) -> String {
    if status.is_empty() {
        return "—".to_string();
    }
    match status {
        "Ended" => "Ended".to_string(),
        "Canceled" | "Cancelled" => "Canceled".to_string(),
        "Returning Series" => "Returning Series".to_string(),
        "In Production" => "In Production".to_string(),
        _ => status.to_string(),
    }
}

fn format_us_long_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    if iso.len() < 10 {
        return iso.to_string();
    }
    let y = &iso[0..4];
    let month_part = &iso[5..7];
    let day_part = &iso[8..10];
    let Ok(mi) = month_part.parse::<usize>() else {
        return iso.to_string();
    };
    let Ok(di) = day_part.parse::<usize>() else {
        return iso.to_string();
    };
    if !(1..=12).contains(&mi) {
        return iso.to_string();
    }
    format!("{} {}, {}", MONTHS[mi - 1], di, y)
}

fn episode_display_rating(vote: f64, vote_count: i32) -> String {
    if vote <= 0.0 {
        return String::new();
    }
    if vote_count > 0 {
        format!("{:.1}/10 ({})", vote, vote_count)
    } else {
        format!("{:.1}/10", vote)
    }
}

fn matched_episodes_in_season(
    sub: &std::collections::HashMap<String, CacheEntry>,
    season_number: i32,
    episode_to_file: &HashMap<i32, String>,
) -> usize {
    let Some(sd) = load_tv_season_details_from_sub(sub, season_number) else {
        return 0;
    };
    sd.episodes
        .iter()
        .filter(|e| episode_to_file.contains_key(&e.id))
        .count()
}

fn build_episode_credits(ep: &Episode, missing_profiles: &mut Vec<String>) -> Vec<TvEpisodeCredit> {
    let mut out: Vec<TvEpisodeCredit> = Vec::new();
    for g in ep.guest_stars.iter().take(4) {
        if out.len() >= 4 {
            break;
        }
        let role = if !g.character.trim().is_empty() {
            g.character.trim().to_string()
        } else {
            "Guest Star".to_string()
        };
        let photo = if !g.profile_path.is_empty() {
            match load_cached_poster(&g.profile_path) {
                Some(img) => img,
                None => {
                    missing_profiles.push(g.profile_path.clone());
                    Default::default()
                }
            }
        } else {
            Default::default()
        };
        out.push(TvEpisodeCredit {
            name: g.name.as_str().into(),
            role: role.as_str().into(),
            photo,
        });
    }
    let mut crew_added = 0usize;
    for c in &ep.crew {
        let job = c.job.trim();
        if job.is_empty() || c.name.trim().is_empty() {
            continue;
        }
        if out.len() >= 8 || crew_added >= 4 {
            break;
        }
        let photo = if !c.profile_path.is_empty() {
            match load_cached_poster(&c.profile_path) {
                Some(img) => img,
                None => {
                    missing_profiles.push(c.profile_path.clone());
                    Default::default()
                }
            }
        } else {
            Default::default()
        };
        out.push(TvEpisodeCredit {
            name: c.name.as_str().into(),
            role: job.into(),
            photo,
        });
        crew_added += 1;
    }
    out
}

fn tv_spoken_names(details: &TVSeriesDetails, max: usize) -> String {
    let mut names: Vec<String> = details
        .spoken_languages
        .iter()
        .filter_map(|s| {
            let n = s.english_name.trim();
            if !n.is_empty() {
                Some(n.to_string())
            } else {
                let n2 = s.name.trim();
                if !n2.is_empty() {
                    Some(n2.to_string())
                } else {
                    None
                }
            }
        })
        .take(max)
        .collect();
    names.sort();
    names.dedup();
    names.join(", ")
}

fn tv_original_language_display(details: &TVSeriesDetails) -> String {
    let code = details.original_language.trim();
    if code.is_empty() {
        return String::new();
    }
    let lower = code.to_lowercase();
    for sl in &details.spoken_languages {
        if sl.iso_639_1.to_lowercase() == lower {
            let name = sl.english_name.trim();
            if !name.is_empty() {
                return format!("{} ({})", name, code.to_uppercase());
            }
        }
    }
    code.to_uppercase()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh_tv_show_ui(
    app: &AppWindow,
    series_id: i32,
    season_idx_override: Option<usize>,
    tree: &Arc<RwLock<UnifiedDirectoryTree>>,
    matched_store: &Arc<MatchedStore>,
    tmdb_store: &Arc<TMDBStore>,
    file_state: &Arc<RwLock<FileStateStore>>,
    tv_seasons_model: &Rc<VecModel<TvSeasonTab>>,
    tv_episodes_model: &Rc<VecModel<TvEpisodeRow>>,
    tv_hero_badges_model: &Rc<VecModel<TvHeroBadge>>,
    tv_detail_items_model: &Rc<VecModel<TvDetailItem>>,
    rt: &Arc<Runtime>,
) {
    let file_state_entries = file_state.read().unwrap().entries().clone();
    use std::collections::HashSet;

    let matched = matched_store.get_matched_snapshot().unwrap_or_default();
    let tmdb_cache = match tmdb_store.get_cache_snapshot() {
        Ok(c) => c,
        Err(_) => crate::storage::tmdb_store::TMDBCache::default(),
    };
    let mut existing_file_ids = HashSet::<String>::new();
    {
        let tree_guard = tree.read().unwrap();
        collect_tree_file_ids(&tree_guard.root, &mut existing_file_ids);
    }

    let mut episode_to_file: HashMap<i32, String> = HashMap::new();
    for (file_id, &episode_id) in &matched.tv {
        if existing_file_ids.contains(file_id) {
            episode_to_file.insert(episode_id, file_id.clone());
        }
    }

    let Some(sub) = tmdb_cache.tv.get(&series_id.to_string()) else {
        tv_seasons_model.set_vec(vec![]);
        tv_episodes_model.set_vec(vec![]);
        tv_hero_badges_model.set_vec(vec![]);
        tv_detail_items_model.set_vec(vec![]);
        app.set_tv_show_has_hero_badges(false);
        app.set_tv_show_series_details_visible(false);
        app.set_tv_show_stats_line("".into());
        app.set_tv_show_first_file_id("".into());
        app.set_tv_show_title("Show not in cache".into());
        app.set_tv_show_overview("".into());
        return;
    };

    let details: TVSeriesDetails = {
        let preferred = sub.get("details_en-US").or_else(|| {
            sub.keys()
                .find(|k| k.starts_with("details_"))
                .and_then(|k| sub.get(k))
        });
        let Some(entry) = preferred else {
            tv_seasons_model.set_vec(vec![]);
            tv_episodes_model.set_vec(vec![]);
            tv_hero_badges_model.set_vec(vec![]);
            tv_detail_items_model.set_vec(vec![]);
            app.set_tv_show_has_hero_badges(false);
            app.set_tv_show_series_details_visible(false);
            app.set_tv_show_stats_line("".into());
            app.set_tv_show_first_file_id("".into());
            app.set_tv_show_title("Details unavailable".into());
            return;
        };
        match serde_json::from_value::<TVSeriesDetails>(entry.data.clone()) {
            Ok(d) => d,
            Err(_) => {
                tv_seasons_model.set_vec(vec![]);
                tv_episodes_model.set_vec(vec![]);
                tv_hero_badges_model.set_vec(vec![]);
                tv_detail_items_model.set_vec(vec![]);
                app.set_tv_show_has_hero_badges(false);
                app.set_tv_show_series_details_visible(false);
                app.set_tv_show_stats_line("".into());
                app.set_tv_show_first_file_id("".into());
                app.set_tv_show_title("Details unavailable".into());
                return;
            }
        }
    };

    let mut season_numbers: Vec<i32> = sub
        .keys()
        .filter_map(|k| parse_season_cache_key(k))
        .collect();
    season_numbers.sort_unstable();
    season_numbers.dedup();

    let mut tabs: Vec<TvSeasonTab> = Vec::new();
    for &sn in &season_numbers {
        let count = matched_episodes_in_season(sub, sn, &episode_to_file);
        let label = if sn == 0 {
            format!("Specials ({count})")
        } else {
            format!("Season {sn} ({count})")
        };
        tabs.push(TvSeasonTab {
            season_number: sn,
            label: label.as_str().into(),
        });
    }
    tv_seasons_model.set_vec(tabs.clone());

    let n_tabs = tabs.len();
    let idx = if n_tabs == 0 {
        0usize
    } else {
        season_idx_override
            .unwrap_or(0)
            .min(n_tabs.saturating_sub(1))
    };
    app.set_tv_show_season_idx(idx as i32);

    let rating_label = if details.vote_average > 0.0 {
        format!("{:.1}", details.vote_average)
    } else {
        "—".to_string()
    };
    let y1 = details.first_air_date.get(..4).unwrap_or("");
    let y2 = details.last_air_date.get(..4).unwrap_or("");
    let years_label = if y1.is_empty() {
        String::new()
    } else if y2.is_empty() || y1 == y2 {
        format!("{y1} – Present")
    } else {
        format!("{y1} – {y2}")
    };

    let lib_eps: usize = season_numbers
        .iter()
        .map(|&sn| matched_episodes_in_season(sub, sn, &episode_to_file))
        .sum();
    let lib_seasons = season_numbers
        .iter()
        .filter(|&&sn| matched_episodes_in_season(sub, sn, &episode_to_file) > 0)
        .count();
    let stats_line = if lib_seasons > 0 || lib_eps > 0 {
        format!(
            "{} season{} · {} episode{}",
            lib_seasons,
            if lib_seasons == 1 { "" } else { "s" },
            lib_eps,
            if lib_eps == 1 { "" } else { "s" },
        )
    } else if details.number_of_seasons > 0 || details.number_of_episodes > 0 {
        format!(
            "{} season{} · {} episode{}",
            details.number_of_seasons,
            if details.number_of_seasons == 1 {
                ""
            } else {
                "s"
            },
            details.number_of_episodes,
            if details.number_of_episodes == 1 {
                ""
            } else {
                "s"
            },
        )
    } else {
        String::new()
    };

    let mut hero_badges: Vec<TvHeroBadge> = Vec::new();
    if let Some(l) = tv_status_badge_label(&details.status) {
        hero_badges.push(TvHeroBadge {
            text: l.into(),
            style: 0,
        });
    }
    if details.in_production {
        hero_badges.push(TvHeroBadge {
            text: "In Production".into(),
            style: 1,
        });
    }
    for g in &details.genres {
        if !g.name.is_empty() {
            hero_badges.push(TvHeroBadge {
                text: g.name.as_str().into(),
                style: 2,
            });
        }
    }
    tv_hero_badges_model.set_vec(hero_badges.clone());
    app.set_tv_show_has_hero_badges(!hero_badges.is_empty());

    let missing_profiles: Vec<String> = Vec::new();
    app.set_tv_show_title(details.name.as_str().into());
    app.set_tv_show_tagline(details.tagline.as_str().into());
    app.set_tv_show_rating_label(rating_label.as_str().into());
    app.set_tv_show_years_label(years_label.as_str().into());
    app.set_tv_show_overview(details.overview.as_str().into());
    app.set_tv_show_stats_line(stats_line.as_str().into());

    let backdrop = if !details.backdrop_path.is_empty() {
        load_cached_backdrop(&details.backdrop_path).unwrap_or_default()
    } else {
        Default::default()
    };
    let hero_poster = if !details.poster_path.is_empty() {
        load_cached_poster(&details.poster_path).unwrap_or_default()
    } else {
        Default::default()
    };
    app.set_tv_show_backdrop(backdrop);
    app.set_tv_show_hero_poster(hero_poster);
    app.set_tv_show_series_id(series_id);

    // Build exactly 12 detail_items (6 per row) so columns stay aligned.
    // Empty label+value acts as a transparent spacer in the grid.
    let slot = |label: &str, value: String| -> TvDetailItem {
        let v = value.trim().to_string();
        if v.is_empty() || v == "—" {
            TvDetailItem {
                label: "".into(),
                value: "".into(),
            }
        } else {
            TvDetailItem {
                label: label.into(),
                value: v.into(),
            }
        }
    };

    // Row 1: Rating · Type · Status · Avg. runtime · Seasons · Episodes
    let rating_val = if details.vote_average > 0.0 {
        let mut r = format!("{:.1}/10", details.vote_average);
        if details.vote_count > 0 {
            r.push_str(&format!(" ({})", details.vote_count));
        }
        r
    } else {
        String::new()
    };

    let type_s = if !details.series_type.trim().is_empty() {
        details.series_type.trim().to_string()
    } else if details.number_of_seasons <= 1
        && details.number_of_episodes <= 20
        && details.number_of_episodes > 0
    {
        "Miniseries".to_string()
    } else {
        "Series".to_string()
    };

    let runtime_val = if !details.episode_run_time.is_empty() {
        let sum: i32 = details.episode_run_time.iter().sum();
        let n = details.episode_run_time.len() as i32;
        let avg = ((f64::from(sum)) / (f64::from(n))).round() as i32;
        format!("{avg}m")
    } else {
        String::new()
    };

    let seasons_val = if details.number_of_seasons > 0 {
        format!("{}", details.number_of_seasons)
    } else {
        String::new()
    };

    let episodes_val = if details.number_of_episodes > 0 {
        format!("{}", details.number_of_episodes)
    } else {
        String::new()
    };

    // Row 2: Spoken · Origin · Original · Popularity · Language · Networks
    let locale_spoken = tv_spoken_names(&details, 3);
    let locale_origin = details.origin_country.join(", ");
    let original_title = {
        let on = details.original_name.trim();
        let tn = details.name.trim();
        if !on.is_empty() && on != tn {
            on.to_string()
        } else {
            String::new()
        }
    };
    let popularity_val = if details.popularity > 0.0 {
        format!("{:.1}", details.popularity)
    } else {
        String::new()
    };
    let lang_disp = tv_original_language_display(&details);
    let nets_label: String = details
        .networks
        .iter()
        .filter(|n| !n.name.trim().is_empty())
        .take(3)
        .map(|n| n.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let detail_items = vec![
        slot("Rating", rating_val),
        slot("Type", type_s),
        slot("Status", tv_status_detail_label(&details.status)),
        slot("Avg. runtime", runtime_val),
        slot("Seasons", seasons_val),
        slot("Episodes", episodes_val),
        slot("Spoken", locale_spoken),
        slot("Origin", locale_origin),
        slot("Original", original_title),
        slot("Popularity", popularity_val),
        slot("Language", lang_disp),
        slot("Networks", nets_label),
    ];

    let show_series_details = detail_items.iter().any(|i| !i.value.is_empty());
    tv_detail_items_model.set_vec(detail_items);
    app.set_tv_show_series_details_visible(show_series_details);

    let sel_sn = tabs.get(idx).map(|t| t.season_number);
    let season_details = sel_sn.and_then(|sn| load_tv_season_details_from_sub(sub, sn));

    if let Some(ref sd) = season_details {
        let block_title: slint::SharedString = if !sd.name.is_empty() {
            sd.name.as_str().into()
        } else if let Some(sn) = sel_sn {
            format!("Season {sn}").into()
        } else {
            "Season".into()
        };
        app.set_tv_show_season_block_title(block_title);

        let air = sd.air_date.as_str();
        let matched_here = sel_sn
            .map(|sn| matched_episodes_in_season(sub, sn, &episode_to_file))
            .unwrap_or(0);
        let meta = format!(
            "{} matched · {} episodes · {}",
            matched_here,
            sd.episodes.len(),
            air
        );
        app.set_tv_show_season_block_meta(meta.as_str().into());
        app.set_tv_show_season_block_overview(sd.overview.as_str().into());

        let sposter = if !sd.poster_path.is_empty() {
            load_cached_poster(&sd.poster_path).unwrap_or_default()
        } else {
            Default::default()
        };
        app.set_tv_show_season_block_poster(sposter);
    } else {
        app.set_tv_show_season_block_title("".into());
        app.set_tv_show_season_block_meta("".into());
        app.set_tv_show_season_block_overview("".into());
        app.set_tv_show_season_block_poster(Default::default());
    }

    let mut ep_rows: Vec<TvEpisodeRow> = Vec::new();
    let mut missing_stills: Vec<String> = Vec::new();
    let mut missing_credit_profiles: Vec<String> = Vec::new();
    if let Some(ref sd) = season_details {
        for ep in &sd.episodes {
            let file_id = episode_to_file.get(&ep.id).cloned().unwrap_or_default();
            if file_id.is_empty() {
                continue;
            }
            let still = if !ep.still_path.is_empty() {
                match load_cached_poster(&ep.still_path) {
                    Some(img) => img,
                    None => {
                        missing_stills.push(ep.still_path.clone());
                        Default::default()
                    }
                }
            } else {
                Default::default()
            };
            let air_fmt = format_us_long_date(ep.air_date.as_str());
            let duration_label = if ep.runtime > 0 {
                format!("{}m", ep.runtime)
            } else {
                String::new()
            };
            let rating_label = episode_display_rating(ep.vote_average, ep.vote_count);
            let credits_vec = build_episode_credits(ep, &mut missing_credit_profiles);
            let credits_m = std::rc::Rc::new(slint::VecModel::from(credits_vec));
            ep_rows.push(TvEpisodeRow {
                ep_num: ep.episode_number,
                title: ep.name.as_str().into(),
                watched: file_state_entries
                    .get(&file_id)
                    .map(|entry| entry.is_completed())
                    .unwrap_or(false),
                air_date: air_fmt.as_str().into(),
                duration_label: duration_label.as_str().into(),
                overview: ep.overview.as_str().into(),
                rating_label: rating_label.as_str().into(),
                still,
                file_id: file_id.as_str().into(),
                credits: slint::ModelRc::from(credits_m.clone()),
            });
        }
    }
    ep_rows.sort_by_key(|r| r.ep_num);
    let first_file_id = ep_rows
        .first()
        .map(|r| r.file_id.to_string())
        .unwrap_or_default();
    app.set_tv_show_first_file_id(first_file_id.as_str().into());
    tv_episodes_model.set_vec(ep_rows);

    // Backdrop is fetched at w1280; everything else at w342.
    if !details.backdrop_path.is_empty() {
        let bp = details.backdrop_path.clone();
        rt.spawn(async move {
            download_backdrop_hd(bp).await;
        });
    }

    let mut missing_fetch = Vec::new();
    if !details.poster_path.is_empty() && load_cached_poster(&details.poster_path).is_none() {
        missing_fetch.push(details.poster_path.clone());
    }
    if let Some(ref sd) = season_details {
        if !sd.poster_path.is_empty() && load_cached_poster(&sd.poster_path).is_none() {
            missing_fetch.push(sd.poster_path.clone());
        }
    }
    missing_fetch.extend(missing_stills);
    missing_fetch.extend(missing_profiles);
    missing_fetch.extend(missing_credit_profiles);
    missing_fetch.sort_unstable();
    missing_fetch.dedup();
    if !missing_fetch.is_empty() {
        let paths = missing_fetch.clone();
        rt.spawn(async move {
            download_posters(paths).await;
        });
    }
}

pub(crate) fn install(
    app: &AppWindow,
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
    let file_state = services.file_state.clone();
    let watch_sync = services.watch_sync.clone();
    let embedded_player = embedded_player.clone();

    app.on_tv_show_back({
        let weak = weak.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                app.set_view(VIEW_MEDIA);
            }
        }
    });

    app.on_tv_show_season_changed({
        let weak = weak.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let file_state = file_state.clone();
        let tv_show_seasons_model = models.tv_seasons.clone();
        let tv_show_episodes_model = models.tv_episodes.clone();
        let tv_show_hero_badges_model = models.tv_hero_badges.clone();
        let tv_show_detail_items_model = models.tv_detail_items.clone();
        let rt = rt.clone();
        move |idx| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let sid = app.get_tv_show_series_id();
            if sid <= 0 {
                return;
            }
            refresh_tv_show_ui(
                &app,
                sid,
                Some(idx.max(0) as usize),
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
        }
    });

    app.on_tv_show_play_episode({
        let weak = weak.clone();
        let tv_show_episodes_model = models.tv_episodes.clone();
        let tv_show_seasons_model = models.tv_seasons.clone();
        let embedded_player = embedded_player.clone();
        move |file_id| {
            let s = file_id.to_string();
            if s.is_empty() {
                return;
            }
            if let Ok(id) = s.parse::<u64>() {
                if let Some(app) = weak.upgrade() {
                    let season_number = tv_show_seasons_model
                        .row_data(app.get_tv_show_season_idx().max(0) as usize)
                        .map(|season| season.season_number)
                        .unwrap_or(0);
                    let rows = (0..tv_show_episodes_model.row_count())
                        .filter_map(|idx| tv_show_episodes_model.row_data(idx))
                        .collect::<Vec<_>>();
                    let start = rows
                        .iter()
                        .position(|row| row.file_id == file_id)
                        .unwrap_or(0);
                    let queue = rows
                        .into_iter()
                        .skip(start)
                        .filter_map(|row| {
                            row.file_id.as_str().parse::<u64>().ok().map(|file_id| {
                                let mut meta = format!("S{season_number:02} E{:02}", row.ep_num);
                                if !row.duration_label.is_empty() {
                                    meta.push_str(" · ");
                                    meta.push_str(row.duration_label.as_str());
                                }
                                PlaybackQueueItem {
                                    file_id,
                                    title: row.title.to_string(),
                                    meta,
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    embedded_player.play_queue(&app, queue, id);
                }
            }
        }
    });

    app.on_tv_show_episode_subtitles({
        let weak = weak.clone();
        move |file_id| {
            let s = file_id.to_string();
            if s.is_empty() {
                return;
            }
            if let Ok(id) = s.parse::<u64>() {
                if let Some(app) = weak.upgrade() {
                    app.invoke_files_menu_action("play".into(), truncate_id(id));
                }
            }
        }
    });

    app.on_tv_show_episode_watch_toggle({
        let weak = weak.clone();
        let file_state = file_state.clone();
        let watch_sync = watch_sync.clone();
        let tree = tree.clone();
        let matched_store = matched_store.clone();
        let tmdb_store = tmdb_store.clone();
        let tv_show_seasons_model = models.tv_seasons.clone();
        let tv_show_episodes_model = models.tv_episodes.clone();
        let tv_show_hero_badges_model = models.tv_hero_badges.clone();
        let tv_show_detail_items_model = models.tv_detail_items.clone();
        let rt = rt.clone();
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
            let Some(app) = weak.upgrade() else {
                return;
            };
            let sid = app.get_tv_show_series_id();
            if sid <= 0 {
                return;
            }
            refresh_tv_show_ui(
                &app,
                sid,
                Some(app.get_tv_show_season_idx().max(0) as usize),
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
            app.invoke_media_refresh();
            app.invoke_request_refresh();
            app.invoke_settings_refresh();
        }
    });
}
