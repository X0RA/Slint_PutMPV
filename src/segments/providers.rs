use super::*;
use crate::{
    metadata::TMDBAPI,
    storage::{config_dir, read_json, write_atomic},
};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const POSITIVE_TTL: u64 = 30 * 86400;
const NEGATIVE_TTL: u64 = 86400;
const ERROR_TTL: u64 = 300;
const MAX_JSON: usize = 1024 * 1024;
const ANIME_LIST: &str =
    "https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-mini.json";

pub struct Service {
    http: reqwest::Client,
    tmdb: Arc<TMDBAPI>,
    access: tokio::sync::Mutex<()>,
    cooldowns: Mutex<HashMap<String, u64>>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    expires: u64,
    segments: Vec<Segment>,
}

impl Service {
    pub fn new(tmdb: Arc<TMDBAPI>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .user_agent("PutMPV/1.0")
                .build()
                .expect("segment HTTP client"),
            tmdb,
            access: tokio::sync::Mutex::new(()),
            cooldowns: Mutex::new(HashMap::new()),
        }
    }

    pub async fn resolve(
        &self,
        file_id: u64,
        episode: &EpisodeContext,
        duration: f64,
        local: Vec<Segment>,
    ) -> Vec<Segment> {
        let _guard = self.access.lock().await;
        let key = cache_key(file_id, episode, duration);
        let path = cache_dir().map(|p| p.join(format!("{key}.json")));
        let mut segments = local;
        if let Some(entry) = path
            .as_ref()
            .and_then(|p| read_json::<CacheEntry>(p).ok().flatten())
            .filter(|e| e.expires > now())
        {
            fill_missing(&mut segments, entry.segments, duration);
            tracing::debug!(file_id, "skip markers loaded from cache");
            return segments;
        }
        // A complete set of file-specific chapters needs no network access.
        if complete(&segments) {
            return segments;
        }
        let mut ids = episode.ids.clone();
        let mut failed = false;
        if episode.source == MetadataSource::Tmdb
            && (ids.imdb_id.is_none() || ids.tvdb_id.is_none())
        {
            match tokio::time::timeout(
                Duration::from_secs(8),
                self.tmdb.get_tv_external_ids(episode.series_id),
            )
            .await
            {
                Ok(Ok(resolved)) => ids = resolved,
                _ => {
                    failed = true;
                }
            }
        }
        if episode.anime && episode.season > 0 {
            match self.anime_mapping(episode, &ids).await {
                Ok(Some(mapping)) => {
                    let url = format!(
                        "https://api.aniskip.com/v2/skip-times/{}/{}",
                        mapping.mal_id, mapping.episode
                    );
                    let query = vec![
                        ("types", "op".into()),
                        ("types", "ed".into()),
                        ("types", "recap".into()),
                        ("episodeLength", format!("{duration:.3}")),
                    ];
                    let result = self
                        .json(&url, &query, MAX_JSON)
                        .await
                        .and_then(|v| v.map(|v| parse_aniskip(&v, duration)).transpose());
                    apply_result(&mut segments, result, duration, &mut failed, "AniSkip");
                }
                Ok(None) => {}
                Err(e) => {
                    failed = true;
                    tracing::debug!("anime skip mapping unavailable: {e}");
                }
            }
        }
        if !complete(&segments)
            && (episode.source == MetadataSource::Tmdb
                || ids.tvdb_id.is_some()
                || valid_imdb(ids.imdb_id.as_deref()))
        {
            let mut query = vec![
                ("season", episode.season.to_string()),
                ("episode", episode.episode.to_string()),
                (
                    "duration_ms",
                    ((duration * 1000.0).round() as u64).to_string(),
                ),
                ("details", "true".into()),
                ("merge_unknown", "false".into()),
                ("episode_name", episode.title.clone()),
            ];
            // TMDB's season/episode coordinates must be resolved by TMDB identity; do not
            // replace them with TVDB coordinates merely because an external ID exists.
            if episode.source == MetadataSource::Tmdb {
                query.push(("tmdb_id", episode.series_id.to_string()));
            } else if let Some(id) = ids.tvdb_id {
                query.push(("tvdb_id", id.to_string()));
                query.push(("mapping", "official".into()));
            } else if let Some(id) = &ids.imdb_id {
                query.push(("imdb_id", id.clone()));
            }
            let result = self
                .json("https://api.theintrodb.org/v4/media", &query, MAX_JSON)
                .await
                .and_then(|v| {
                    v.map(|v| parse_theintrodb(&v, episode, &ids, duration))
                        .transpose()
                });
            apply_result(&mut segments, result, duration, &mut failed, "TheIntroDB");
        }
        if !complete(&segments) && valid_imdb(ids.imdb_id.as_deref()) {
            let imdb = ids.imdb_id.as_deref().unwrap();
            let query = [
                ("imdb_id", imdb.into()),
                ("season", episode.season.to_string()),
                ("episode", episode.episode.to_string()),
            ];
            let result = self
                .json("https://api.introdb.app/segments", &query, MAX_JSON)
                .await
                .and_then(|v| {
                    v.map(|v| parse_introdb(&v, episode, imdb, duration))
                        .transpose()
                });
            apply_result(&mut segments, result, duration, &mut failed, "IntroDB");
        }
        // File chapters are re-read from the actual stream, never reused for a transcode.
        let remote: Vec<_> = segments
            .iter()
            .filter(|s| s.source != "Chapters")
            .cloned()
            .collect();
        let ttl = cache_ttl(!remote.is_empty(), failed);
        if let Some(path) = path {
            if let Err(e) = write_atomic(
                &path,
                &CacheEntry {
                    expires: now() + ttl,
                    segments: remote,
                },
            ) {
                tracing::debug!("could not cache skip markers: {e}");
            }
        }
        tracing::debug!(
            file_id,
            count = segments.len(),
            "skip marker lookup finished"
        );
        segments
    }

    async fn json(
        &self,
        url: &str,
        query: &[(&str, String)],
        limit: usize,
    ) -> Result<Option<Value>> {
        let host = reqwest::Url::parse(url)?
            .host_str()
            .unwrap_or_default()
            .to_string();
        if self
            .cooldowns
            .lock()
            .unwrap()
            .get(&host)
            .is_some_and(|until| *until > now())
        {
            bail!("provider is cooling down");
        }
        let mut response = self
            .http
            .get(url)
            .timeout(Duration::from_secs(if limit > MAX_JSON { 20 } else { 8 }))
            .query(query)
            .send()
            .await
            .map_err(|e| e.without_url())?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(300)
                .clamp(1, 86400);
            self.cooldowns.lock().unwrap().insert(host, now() + retry);
        }
        if !status.is_success() {
            bail!("provider returned {status}");
        }
        if response.content_length().is_some_and(|n| n > limit as u64) {
            bail!("response too large");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
            if bytes.len() + chunk.len() > limit {
                bail!("response too large");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    async fn anime_mapping(
        &self,
        episode: &EpisodeContext,
        ids: &ExternalIds,
    ) -> Result<Option<AnimeMapping>> {
        let path = cache_dir().map(|p| p.join("anime-ids.json"));
        let cached = path
            .as_ref()
            .and_then(|p| read_json::<AnimeCache>(p).ok().flatten());
        let entries = if let Some(cached) = cached.as_ref().filter(|c| c.expires > now()) {
            cached.entries.clone()
        } else {
            match self.json(ANIME_LIST, &[], 20 * MAX_JSON).await {
                Ok(Some(value)) => {
                    let entries: Vec<AnimeEntry> = serde_json::from_value(value)?;
                    if let Some(path) = path {
                        let _ = write_atomic(
                            &path,
                            &AnimeCache {
                                expires: now() + 7 * 86400,
                                entries: entries.clone(),
                            },
                        );
                    }
                    entries
                }
                Ok(None) => cached
                    .map(|c| c.entries)
                    .ok_or_else(|| anyhow!("anime mapping unavailable"))?,
                Err(e) => cached.map(|c| c.entries).ok_or(e)?,
            }
        };
        Ok(map_anime(&entries, episode, ids))
    }
}

fn cache_dir() -> Option<PathBuf> {
    config_dir().ok().map(|p| p.join("segments"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn cache_ttl(has_markers: bool, failed: bool) -> u64 {
    if failed {
        ERROR_TTL
    } else if has_markers {
        POSITIVE_TTL
    } else {
        NEGATIVE_TTL
    }
}
fn cache_key(file_id: u64, episode: &EpisodeContext, duration: f64) -> String {
    // Enriching external IDs must not invalidate the markers just fetched for this file.
    let bytes = serde_json::to_vec(&(
        4,
        file_id,
        episode.source,
        episode.series_id,
        episode.episode_id,
        episode.season,
        episode.episode,
        &episode.file_version,
        (duration * 1000.0).round() as u64,
    ))
    .unwrap();
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_imdb(id: Option<&str>) -> bool {
    id.and_then(|s| s.strip_prefix("tt"))
        .is_some_and(|s| s.len() >= 7 && s.len() <= 10 && s.bytes().all(|c| c.is_ascii_digit()))
}
fn complete(segments: &[Segment]) -> bool {
    [Kind::Intro, Kind::Recap, Kind::Credits]
        .iter()
        .all(|k| segments.iter().any(|s| s.kind == *k))
}
fn apply_result(
    segments: &mut Vec<Segment>,
    result: Result<Option<Vec<Segment>>>,
    duration: f64,
    failed: &mut bool,
    provider: &str,
) {
    match result {
        Ok(Some(found)) => {
            tracing::debug!(
                provider,
                count = found.len(),
                "skip provider returned markers"
            );
            fill_missing(segments, found, duration);
        }
        Ok(None) => {}
        Err(e) => {
            *failed = true;
            tracing::debug!(provider, "skip lookup unavailable: {e}");
        }
    }
}

fn runtime_matches(reference: f64, duration: f64) -> bool {
    reference.is_finite() && reference > 0.0 && (reference - duration).abs() <= 5.0
}
fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}
fn identity_matches(v: &Value, ep: &EpisodeContext) -> bool {
    v.get("season").and_then(Value::as_i64) == Some(ep.season as i64)
        && v.get("episode").and_then(Value::as_i64) == Some(ep.episode as i64)
}

fn parse_theintrodb(
    v: &Value,
    ep: &EpisodeContext,
    ids: &ExternalIds,
    duration: f64,
) -> Result<Vec<Segment>> {
    if v.get("type").and_then(Value::as_str) != Some("tv") || !identity_matches(v, ep) {
        bail!("episode identity mismatch");
    }
    let expected_tvdb = ids.tvdb_id;
    if let Some(expected) = expected_tvdb {
        if v.get("tvdb_id").and_then(Value::as_i64) != Some(expected as i64) {
            bail!("series identity mismatch");
        }
    } else if let Some(id) = v.get("tmdb_id").and_then(Value::as_i64) {
        if ep.source != MetadataSource::Tmdb || id != ep.series_id as i64 {
            bail!("series identity mismatch");
        }
    } else if v
        .get("tvdb_id")
        .and_then(Value::as_i64)
        .is_none_or(|id| id <= 0)
    {
        bail!("missing series identity");
    }
    // Unknown-duration submissions were excluded from the request. Require a release match.
    let reference =
        number(v, "group_duration_ms").ok_or_else(|| anyhow!("unknown release duration"))? / 1000.0;
    if !runtime_matches(reference, duration) {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for (field, kind) in [
        ("intro", Kind::Intro),
        ("recap", Kind::Recap),
        ("credits", Kind::Credits),
    ] {
        for item in v.get(field).and_then(Value::as_array).into_iter().flatten() {
            if number(item, "confidence").is_some_and(|c| !(0.15..=1.0).contains(&c)) {
                continue;
            }
            let start = match item.get("start_ms") {
                Some(Value::Null) if kind != Kind::Credits => 0.0,
                _ => match number(item, "start_ms") {
                    Some(n) => n / 1000.0,
                    None => continue,
                },
            };
            let end = match item.get("end_ms") {
                Some(Value::Null) if kind == Kind::Credits => duration,
                _ => match number(item, "end_ms") {
                    Some(n) => n / 1000.0,
                    None => continue,
                },
            };
            result.push(Segment {
                kind,
                start,
                end,
                source: "TheIntroDB".into(),
            });
        }
    }
    Ok(result)
}

fn parse_introdb(
    v: &Value,
    ep: &EpisodeContext,
    imdb: &str,
    duration: f64,
) -> Result<Vec<Segment>> {
    if v.get("imdb_id").and_then(Value::as_str) != Some(imdb)
        || !identity_matches(v, ep)
        || v.get("is_movie").and_then(Value::as_bool) == Some(true)
    {
        bail!("episode identity mismatch");
    }
    let mut result = Vec::new();
    for (field, kind) in [
        ("intro", Kind::Intro),
        ("recap", Kind::Recap),
        ("outro", Kind::Credits),
    ] {
        let Some(item) = v.get(field).filter(|i| i.is_object()) else {
            continue;
        };
        if number(item, "confidence").is_some_and(|c| !(0.5..=1.0).contains(&c)) {
            continue;
        }
        let Some(start) = number(item, "start_ms")
            .map(|n| n / 1000.0)
            .or_else(|| number(item, "start_sec"))
        else {
            continue;
        };
        let Some(end) = number(item, "end_ms")
            .map(|n| n / 1000.0)
            .or_else(|| number(item, "end_sec"))
        else {
            continue;
        };
        let segment = Segment {
            kind,
            start,
            end,
            source: "IntroDB".into(),
        };
        // This provider cannot prove the cut. Use plausible, manual-only markers.
        if segment.valid(duration) {
            result.push(segment);
        }
    }
    Ok(result)
}

fn parse_aniskip(v: &Value, duration: f64) -> Result<Vec<Segment>> {
    if v.get("found").and_then(Value::as_bool) == Some(false) {
        return Ok(Vec::new());
    }
    let results = v
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("invalid AniSkip response"))?;
    let mut segments = Vec::new();
    for item in results {
        if !number(item, "episodeLength").is_some_and(|n| runtime_matches(n, duration)) {
            continue;
        }
        let kind = match item.get("skipType").and_then(Value::as_str) {
            Some("op") => Kind::Intro,
            Some("recap") => Kind::Recap,
            Some("ed") => Kind::Credits,
            _ => continue,
        };
        let Some(interval) = item.get("interval") else {
            continue;
        };
        if let (Some(start), Some(end)) =
            (number(interval, "startTime"), number(interval, "endTime"))
        {
            segments.push(Segment {
                kind,
                start,
                end,
                source: "AniSkip".into(),
            });
        }
    }
    Ok(segments)
}

#[derive(Clone, Serialize, Deserialize)]
struct AnimeEntry {
    #[serde(default)]
    mal_id: Option<u32>,
    #[serde(default)]
    themoviedb_id: HashMap<String, Value>,
    #[serde(default)]
    tvdb_id: Option<u32>,
    #[serde(default)]
    season: HashMap<String, Value>,
    #[serde(default)]
    episode_offset: HashMap<String, Value>,
}
#[derive(Serialize, Deserialize)]
struct AnimeCache {
    expires: u64,
    entries: Vec<AnimeEntry>,
}
#[derive(Debug, PartialEq)]
struct AnimeMapping {
    mal_id: u32,
    episode: u32,
}
fn map_anime(
    entries: &[AnimeEntry],
    ep: &EpisodeContext,
    ids: &ExternalIds,
) -> Option<AnimeMapping> {
    let provider = if ep.source == MetadataSource::Tmdb {
        "tmdb"
    } else {
        "tvdb"
    };
    let matches: Vec<_> = entries
        .iter()
        .filter(|entry| {
            let series = if provider == "tmdb" {
                entry.themoviedb_id.get("tv").and_then(Value::as_i64) == Some(ep.series_id as i64)
            } else {
                ids.tvdb_id
                    .is_some_and(|id| entry.tvdb_id == Some(id as u32))
            };
            series && entry.season.get(provider).and_then(Value::as_i64) == Some(ep.season as i64)
        })
        .collect();
    // Split cours and custom episode mappings need a richer mapping, never guess by title.
    if matches.len() != 1 {
        return None;
    }
    let entry = matches[0];
    let offset = match entry.episode_offset.get(provider) {
        None => 0,
        Some(v) => v.as_i64()?,
    };
    let number = (ep.episode as i64).checked_sub(offset)?;
    if number <= 0 {
        return None;
    }
    Some(AnimeMapping {
        mal_id: entry.mal_id.filter(|id| *id > 0)?,
        episode: u32::try_from(number).ok()?,
    })
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
