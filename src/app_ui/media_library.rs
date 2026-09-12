//! Provider-aware snapshots for the shared media and episode views.
//!
//! Positive view IDs belong to TMDB; negative IDs belong to TVMaze. This
//! namespace is only used in memory: persisted matches retain their source
//! and original provider IDs, and playback always uses the real put.io ID.
use crate::metadata::tmdb::{Episode, Genre, Season, TVSeasonDetails, TVSeriesDetails};
use crate::metadata::tvmaze::{TVMazeEpisode, TVMazeSeason, TVMazeShow};
use crate::storage::matched_store::MatchedData;
use crate::storage::tmdb_store::{CacheEntry, TMDBCache};
use crate::storage::tvmaze_store::TVMazeCache;

pub(crate) fn merge_tvmaze(cache: &mut TMDBCache, matched: &mut MatchedData, maze: &TVMazeCache) {
    for (file_id, episode_id) in &mut matched.tv {
        if matched
            .tv_source
            .get(file_id)
            .is_some_and(|s| s == "tvmaze")
        {
            *episode_id = episode_id
                .checked_abs()
                .and_then(i32::checked_neg)
                .unwrap_or(0);
        }
    }
    for (key, entry) in &maze.data {
        if !key.starts_with("show_") || !key.ends_with("_details") {
            continue;
        }
        let Ok(show) = serde_json::from_value::<TVMazeShow>(entry.data.clone()) else {
            continue;
        };
        if show.id <= 0 {
            continue;
        }
        let seasons: Vec<TVMazeSeason> = maze
            .data
            .get(&format!("show_{}_seasons", show.id))
            .and_then(|e| serde_json::from_value(e.data.clone()).ok())
            .unwrap_or_default();
        let mut details = TVSeriesDetails {
            id: -show.id,
            name: show.name.clone(),
            original_name: show.name,
            overview: plain_summary(&show.summary),
            first_air_date: show.premiered,
            last_air_date: show.ended,
            poster_path: show
                .image
                .map(|i| {
                    if i.original.is_empty() {
                        i.medium
                    } else {
                        i.original
                    }
                })
                .unwrap_or_default(),
            status: show.status,
            series_type: show.r#type,
            episode_run_time: show.runtime.into_iter().collect(),
            genres: show
                .genres
                .into_iter()
                .map(|name| Genre { id: 0, name })
                .collect(),
            number_of_seasons: seasons.len() as i32,
            ..Default::default()
        };
        let sub = cache.tv.entry((-show.id).to_string()).or_default();
        for season in seasons {
            let episodes: Vec<TVMazeEpisode> = maze
                .data
                .get(&format!("season_{}_episodes", season.id))
                .and_then(|e| serde_json::from_value(e.data.clone()).ok())
                .unwrap_or_default();
            let episodes: Vec<Episode> = episodes
                .into_iter()
                .filter(|e| e.id > 0)
                .map(|e| Episode {
                    id: -e.id,
                    name: e.name,
                    overview: plain_summary(&e.summary),
                    air_date: e.airdate,
                    episode_number: e.number,
                    season_number: e.season,
                    runtime: e.runtime.unwrap_or_default(),
                    still_path: e
                        .image
                        .map(|i| {
                            if i.original.is_empty() {
                                i.medium
                            } else {
                                i.original
                            }
                        })
                        .unwrap_or_default(),
                    ..Default::default()
                })
                .collect();
            details.number_of_episodes += episodes.len() as i32;
            details.seasons.push(Season {
                season_number: season.number,
                episode_count: episodes.len() as i32,
                ..Default::default()
            });
            let sd = TVSeasonDetails {
                id: -season.id,
                season_number: season.number,
                name: season.name,
                air_date: season.premiere_date,
                episode_count: episodes.len() as i32,
                episodes,
                ..Default::default()
            };
            match serde_json::to_value(sd) {
                Ok(data) => {
                    sub.insert(
                        format!("season_{}_en-US", season.number),
                        CacheEntry { data },
                    );
                }
                Err(error) => {
                    tracing::warn!("could not convert TVMaze season for display: {error}")
                }
            }
        }
        match serde_json::to_value(details) {
            Ok(data) => {
                sub.insert("details_en-US".into(), CacheEntry { data });
            }
            Err(error) => tracing::warn!("could not convert TVMaze show for display: {error}"),
        }
    }
}

fn plain_summary(value: &str) -> String {
    static TAGS: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new("<[^>]*>").unwrap());
    static ENTITIES: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"&(#(?:[xX][0-9a-fA-F]+|\d+)|amp|quot|apos|lt|gt|nbsp);").unwrap()
    });
    let text = TAGS.replace_all(value, " ");
    // Decode once, so encoded entity text such as &amp;quot; stays &quot;.
    ENTITIES
        .replace_all(&text, |caps: &regex::Captures<'_>| {
            let entity = &caps[1];
            let decoded = match entity {
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| {
                        entity
                            .strip_prefix('#')
                            .and_then(|decimal| decimal.parse().ok())
                    })
                    .and_then(char::from_u32),
            };
            decoded
                .map(|c| c.to_string())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn summary_entities_are_decoded_once_after_stripping_tags() {
        assert_eq!(
            plain_summary("<p>&amp;quot; &quot; &lt;b&gt; &#x27; &#39; &unknown;</p>"),
            r#"&quot; " <b> ' ' &unknown;"#
        );
    }

    #[test]
    fn tvmaze_ids_do_not_collide_with_tmdb_and_keep_playable_file_ids() {
        let mut cache = TMDBCache::default();
        cache.tv.insert("42".into(), Default::default());
        let mut matches = MatchedData::default();
        matches.tv.insert("100".into(), 7);
        matches.tv.insert("200".into(), 7);
        matches.tv_source.insert("200".into(), "tvmaze".into());
        let mut maze = TVMazeCache::default();
        for (key, data) in [
            (
                "show_42_details",
                json!({"id":42,"name":"Maze Show","summary":"<p>Hello</p>"}),
            ),
            ("show_42_seasons", json!([{"id":9,"number":1}])),
            (
                "season_9_episodes",
                json!([{"id":7,"number":1,"season":1,"name":"Pilot"}]),
            ),
        ] {
            maze.data.insert(key.into(), CacheEntry { data });
        }
        merge_tvmaze(&mut cache, &mut matches, &maze);
        assert_eq!(matches.tv["100"], 7);
        assert_eq!(matches.tv["200"], -7);
        assert!(cache.tv.contains_key("42"));
        let season: TVSeasonDetails =
            serde_json::from_value(cache.tv["-42"]["season_1_en-US"].data.clone()).unwrap();
        assert_eq!(season.episodes[0].id, -7);
        assert_eq!(season.episodes[0].name, "Pilot");
    }
}
