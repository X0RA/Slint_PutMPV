use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{config_dir, read_json, write_atomic};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheEntry {
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TMDBCache {
    #[serde(default)]
    pub movies: HashMap<String, HashMap<String, CacheEntry>>,
    #[serde(default)]
    pub tv: HashMap<String, HashMap<String, CacheEntry>>,
}

#[derive(Debug)]
pub struct TMDBStore {
    path: PathBuf,
}

impl TMDBStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("tmdb.json");
        if read_json::<TMDBCache>(&path)?.is_none() {
            write_atomic(&path, &TMDBCache::default())?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn get_cached_data(&self, key: &str) -> Option<Value> {
        let cache = self.read_cache().ok()?;
        let (media_type, id, cache_key) = split_tmdb_key(key).ok()?;
        match media_type {
            "movie" => cache.movies.get(id)?.get(cache_key).map(|e| e.data.clone()),
            "tv" => cache.tv.get(id)?.get(cache_key).map(|e| e.data.clone()),
            _ => None,
        }
    }

    pub fn set_cached_data(&self, key: &str, data: Value) -> Result<()> {
        let mut cache = self.read_cache()?;
        let (media_type, id, cache_key) = split_tmdb_key(key)?;
        let entry = CacheEntry { data };
        match media_type {
            "movie" => {
                cache
                    .movies
                    .entry(id.to_string())
                    .or_default()
                    .insert(cache_key.to_string(), entry);
            }
            "tv" => {
                cache
                    .tv
                    .entry(id.to_string())
                    .or_default()
                    .insert(cache_key.to_string(), entry);
            }
            _ => return Err(anyhow!("invalid media type: {media_type}")),
        }
        self.save_cache(&cache)
    }

    pub fn get_cache_snapshot(&self) -> Result<TMDBCache> {
        self.read_cache()
    }

    pub fn clear_cache(&self) -> Result<()> {
        self.save_cache(&TMDBCache::default())
    }

    #[allow(dead_code)]
    pub fn clear_movie_cache(&self, movie_id: i32) -> Result<()> {
        let mut cache = self.read_cache()?;
        cache.movies.remove(&movie_id.to_string());
        self.save_cache(&cache)
    }

    #[allow(dead_code)]
    pub fn clear_tv_cache(&self, series_id: i32) -> Result<()> {
        let mut cache = self.read_cache()?;
        cache.tv.remove(&series_id.to_string());
        self.save_cache(&cache)
    }

    fn read_cache(&self) -> Result<TMDBCache> {
        Ok(read_json::<TMDBCache>(&self.path)?.unwrap_or_default())
    }

    fn save_cache(&self, cache: &TMDBCache) -> Result<()> {
        write_atomic(&self.path, cache)
    }
}

fn split_tmdb_key(key: &str) -> Result<(&str, &str, &str)> {
    let mut parts = key.splitn(3, '_');
    let media_type = parts
        .next()
        .ok_or_else(|| anyhow!("invalid cache key format: {key}"))?;
    let id = parts
        .next()
        .ok_or_else(|| anyhow!("invalid cache key format: {key}"))?;
    let cache_key = parts
        .next()
        .ok_or_else(|| anyhow!("invalid cache key format: {key}"))?;
    Ok((media_type, id, cache_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splits_movie_and_tv_cache() {
        let dir = tempfile_path("tmdb_store_split");
        let store = TMDBStore {
            path: dir.join("tmdb.json"),
        };

        store
            .set_cached_data("movie_10_details_en-US", json!({"id": 10}))
            .unwrap();
        store
            .set_cached_data("tv_20_season_1_en-US", json!({"id": 30}))
            .unwrap();

        assert_eq!(
            store.get_cached_data("movie_10_details_en-US").unwrap()["id"],
            10
        );
        assert_eq!(
            store.get_cached_data("tv_20_season_1_en-US").unwrap()["id"],
            30
        );

        let snap = store.get_cache_snapshot().unwrap();
        assert!(snap.movies["10"].contains_key("details_en-US"));
        assert!(snap.tv["20"].contains_key("season_1_en-US"));
    }

    fn tempfile_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "putmpv_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
