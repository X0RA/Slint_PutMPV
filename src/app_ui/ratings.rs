//! TMDB and IMDb rating labels. IMDb ratings are optional: any missing ID or
//! rating yields an empty label, which the UI hides.
use std::collections::HashMap;

use crate::metadata::tmdb::episode_external_ids_subkey;
use crate::segments::ExternalIds;
use crate::storage::imdb_store::ImdbCache;
use crate::storage::tmdb_store::CacheEntry;

/// "9.8K" style vote counts, matching how IMDb abbreviates them.
pub(crate) fn compact_votes(votes: u64) -> String {
    let scaled = |n: u64, unit: f64, suffix: &str| {
        let v = n as f64 / unit;
        if v < 10.0 {
            let s = format!("{v:.1}");
            format!("{}{suffix}", s.trim_end_matches(".0"))
        } else {
            format!("{}{suffix}", v.round() as u64)
        }
    };
    match votes {
        0..1_000 => votes.to_string(),
        1_000..999_500 => scaled(votes, 1_000.0, "K"),
        _ => scaled(votes, 1_000_000.0, "M"),
    }
}

/// "7.6" without votes, "7.6 (9.8K)" with them, or empty when unrated.
pub(crate) fn rating_label(rating: f64, votes: Option<u64>) -> String {
    if rating <= 0.0 {
        return String::new();
    }
    match votes.filter(|v| *v > 0) {
        Some(v) => format!("{rating:.1} ({})", compact_votes(v)),
        None => format!("{rating:.1}"),
    }
}

pub(crate) fn tmdb_label(vote_average: f64, vote_count: i32, with_votes: bool) -> String {
    let votes = (with_votes && vote_count > 0).then_some(vote_count as u64);
    rating_label(vote_average, votes)
}

pub(crate) fn imdb_label(cache: &ImdbCache, imdb_id: &str, with_votes: bool) -> String {
    if imdb_id.is_empty() {
        return String::new();
    }
    let Some(entry) = cache.ratings.get(imdb_id) else {
        return String::new();
    };
    let Some(rating) = entry.rating else {
        return String::new();
    };
    rating_label(rating, entry.votes.filter(|_| with_votes))
}

fn external_imdb_id(sub: &HashMap<String, CacheEntry>, key: &str) -> String {
    sub.get(key)
        .and_then(|e| serde_json::from_value::<ExternalIds>(e.data.clone()).ok())
        .and_then(|ids| ids.imdb_id)
        .unwrap_or_default()
}

/// IMDb ID of a show from its TMDB cache entries.
pub(crate) fn show_imdb_id(sub: &HashMap<String, CacheEntry>) -> String {
    external_imdb_id(sub, "external_ids")
}

/// IMDb ID of an episode from its show's TMDB cache entries.
pub(crate) fn episode_imdb_id(
    sub: &HashMap<String, CacheEntry>,
    season_number: i32,
    episode_number: i32,
) -> String {
    external_imdb_id(
        sub,
        &episode_external_ids_subkey(season_number, episode_number),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::imdb_store::ImdbRating;
    use serde_json::json;

    #[test]
    fn compacts_vote_counts_like_imdb() {
        assert_eq!(compact_votes(842), "842");
        assert_eq!(compact_votes(9_032), "9K");
        assert_eq!(compact_votes(9_821), "9.8K");
        assert_eq!(compact_votes(11_860), "12K");
        assert_eq!(compact_votes(509_996), "510K");
        assert_eq!(compact_votes(999_600), "1M");
        assert_eq!(compact_votes(2_340_000), "2.3M");
    }

    #[test]
    fn rating_labels_hide_unrated_titles() {
        assert_eq!(rating_label(0.0, Some(10)), "");
        assert_eq!(rating_label(7.6, Some(9_821)), "7.6 (9.8K)");
        assert_eq!(rating_label(8.0, Some(0)), "8.0");
        assert_eq!(tmdb_label(7.42, 42, true), "7.4 (42)");
        assert_eq!(tmdb_label(7.42, 42, false), "7.4");
    }

    #[test]
    fn imdb_labels_are_empty_when_unknown() {
        let cache = ImdbCache {
            ratings: HashMap::from([
                (
                    "tt1".to_string(),
                    ImdbRating {
                        rating: Some(9.1),
                        votes: Some(86_770),
                        fetched_at: 1,
                    },
                ),
                ("tt2".to_string(), ImdbRating::default()),
            ]),
        };
        assert_eq!(imdb_label(&cache, "tt1", true), "9.1 (87K)");
        assert_eq!(imdb_label(&cache, "tt1", false), "9.1");
        assert_eq!(imdb_label(&cache, "tt2", true), "");
        assert_eq!(imdb_label(&cache, "tt3", true), "");
        assert_eq!(imdb_label(&cache, "", true), "");
    }

    #[test]
    fn reads_show_and_episode_imdb_ids_from_tmdb_cache() {
        let entry = |data| CacheEntry { data };
        let sub = HashMap::from([
            (
                "external_ids".to_string(),
                entry(json!({"imdb_id": "tt10986410", "tvdb_id": 383203})),
            ),
            (
                "episode_external_ids_2_1".to_string(),
                entry(json!({"imdb_id": "tt12926006", "tvdb_id": null})),
            ),
            (
                "episode_external_ids_2_2".to_string(),
                entry(json!({"imdb_id": null})),
            ),
        ]);
        assert_eq!(show_imdb_id(&sub), "tt10986410");
        assert_eq!(episode_imdb_id(&sub, 2, 1), "tt12926006");
        assert_eq!(episode_imdb_id(&sub, 2, 2), "");
        assert_eq!(episode_imdb_id(&sub, 3, 1), "");
    }
}
