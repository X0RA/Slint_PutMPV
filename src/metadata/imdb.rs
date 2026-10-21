//! IMDb ratings from the public Agregarr proxy (no API key). Best effort only:
//! callers ignore failures and the UI simply hides missing ratings.
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::storage::imdb_store::{ImdbCache, ImdbRating, ImdbStore};

const RATINGS_URL: &str = "https://api.agregarr.org/api/ratings";
/// The service accepts at most 100 IDs per request.
const BATCH_SIZE: usize = 100;
/// IMDb publishes ratings once a day.
const MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct ImdbRatingsAPI {
    http: Client,
    store: Arc<ImdbStore>,
}

#[derive(Debug, Deserialize)]
struct RatingResponse {
    #[serde(rename = "imdbId")]
    imdb_id: String,
    #[serde(default)]
    rating: Option<f64>,
    #[serde(default)]
    votes: Option<u64>,
}

impl ImdbRatingsAPI {
    pub fn new(store: Arc<ImdbStore>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("PutMPV/1.0")
            .build()
            .expect("reqwest client build");
        Self { http, store }
    }

    /// Fetches ratings that are missing or older than a day. Returns true when
    /// any stored rating changed.
    pub async fn refresh(&self, ids: &[String]) -> Result<bool> {
        let cache = self.store.get_cache_snapshot().unwrap_or_default();
        let stale = stale_ids(&cache, ids, now_unix());
        if stale.is_empty() {
            return Ok(false);
        }
        let mut changed = false;
        for batch in stale.chunks(BATCH_SIZE) {
            let fetched = self.fetch_batch(batch).await?;
            changed |= fetched.iter().any(|(id, new)| {
                cache
                    .ratings
                    .get(id)
                    .is_none_or(|old| old.rating != new.rating || old.votes != new.votes)
            });
            self.store.set_ratings(fetched)?;
        }
        Ok(changed)
    }

    async fn fetch_batch(&self, ids: &[String]) -> Result<HashMap<String, ImdbRating>> {
        let query = ids.iter().map(|id| ("id", id.as_str())).collect::<Vec<_>>();
        let resp = self.http.get(RATINGS_URL).query(&query).send().await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status != StatusCode::OK {
            return Err(anyhow!("IMDb ratings request failed with status {status}"));
        }
        Ok(parse_ratings(&body, now_unix())?)
    }
}

pub fn is_valid_imdb_id(id: &str) -> bool {
    id.strip_prefix("tt")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn stale_ids(cache: &ImdbCache, ids: &[String], now: u64) -> Vec<String> {
    ids.iter()
        .filter(|id| is_valid_imdb_id(id))
        .filter(|id| {
            cache
                .ratings
                .get(id.as_str())
                .is_none_or(|r| now.saturating_sub(r.fetched_at) >= MAX_AGE_SECS)
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_ratings(body: &[u8], now: u64) -> serde_json::Result<HashMap<String, ImdbRating>> {
    let rows: Vec<RatingResponse> = serde_json::from_slice(body)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.imdb_id,
                ImdbRating {
                    rating: row.rating.filter(|r| *r > 0.0),
                    votes: row.votes,
                    fetched_at: now,
                },
            )
        })
        .collect())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(fetched_at: u64) -> ImdbRating {
        ImdbRating {
            rating: Some(8.0),
            votes: Some(100),
            fetched_at,
        }
    }

    #[test]
    fn validates_imdb_ids() {
        assert!(is_valid_imdb_id("tt0959621"));
        assert!(!is_valid_imdb_id("tt"));
        assert!(!is_valid_imdb_id("nm0000001"));
        assert!(!is_valid_imdb_id("tt12a"));
        assert!(!is_valid_imdb_id(""));
    }

    #[test]
    fn stale_ids_skip_fresh_and_invalid_entries_and_deduplicate() {
        let now = 1_000_000;
        let cache = ImdbCache {
            ratings: HashMap::from([
                ("tt1".to_string(), cached(now - 60)),
                ("tt2".to_string(), cached(now - MAX_AGE_SECS)),
            ]),
        };
        let ids = ["tt1", "tt2", "tt3", "tt3", "bogus", ""]
            .map(String::from)
            .to_vec();
        assert_eq!(stale_ids(&cache, &ids, now), vec!["tt2", "tt3"]);
    }

    #[test]
    fn parses_agregarr_response_including_unknown_titles() {
        let body = br#"[
            {"imdbId":"tt12926006","rating":7.6,"votes":9821},
            {"imdbId":"tt99999999","rating":null,"votes":null}
        ]"#;
        let parsed = parse_ratings(body, 42).unwrap();
        assert_eq!(
            parsed["tt12926006"],
            ImdbRating {
                rating: Some(7.6),
                votes: Some(9821),
                fetched_at: 42
            }
        );
        assert_eq!(parsed["tt99999999"].rating, None);
        assert_eq!(parsed["tt99999999"].fetched_at, 42);
    }
}
