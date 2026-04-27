use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tmdb_store::CacheEntry;
use super::{config_dir, read_json, write_atomic};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeCache {
    #[serde(default)]
    pub data: HashMap<String, CacheEntry>,
}

#[derive(Debug)]
pub struct TVMazeStore {
    path: PathBuf,
}

impl TVMazeStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("tvmaze.json");
        if read_json::<TVMazeCache>(&path)?.is_none() {
            write_atomic(&path, &TVMazeCache::default())?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn get_cached_data(&self, key: &str) -> Option<Value> {
        let cache = self.read_cache().ok()?;
        cache.data.get(key).map(|e| e.data.clone())
    }

    pub fn set_cached_data(&self, key: &str, data: Value) -> Result<()> {
        let mut cache = self.read_cache()?;
        cache.data.insert(key.to_string(), CacheEntry { data });
        self.save_cache(&cache)
    }

    #[allow(dead_code)]
    pub fn get_cache_snapshot(&self) -> Result<TVMazeCache> {
        self.read_cache()
    }

    pub fn clear_cache(&self) -> Result<()> {
        self.save_cache(&TVMazeCache::default())
    }

    #[allow(dead_code)]
    pub fn clear_show_cache(&self, show_id: i32) -> Result<()> {
        let mut cache = self.read_cache()?;
        let prefix = format!("show_{show_id}_");
        cache.data.retain(|key, _| !key.starts_with(&prefix));
        self.save_cache(&cache)
    }

    fn read_cache(&self) -> Result<TVMazeCache> {
        Ok(read_json::<TVMazeCache>(&self.path)?.unwrap_or_default())
    }

    fn save_cache(&self, cache: &TVMazeCache) -> Result<()> {
        write_atomic(&self.path, cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clear_show_cache_removes_only_show_keys() {
        let dir = tempfile_path("tvmaze_store_clear");
        let store = TVMazeStore {
            path: dir.join("tvmaze.json"),
        };
        store
            .set_cached_data("show_10_details", json!({"id": 10}))
            .unwrap();
        store
            .set_cached_data("show_10_seasons", json!([{"id": 1}]))
            .unwrap();
        store
            .set_cached_data("season_99_episodes", json!([{"id": 2}]))
            .unwrap();

        store.clear_show_cache(10).unwrap();

        assert!(store.get_cached_data("show_10_details").is_none());
        assert!(store.get_cached_data("show_10_seasons").is_none());
        assert!(store.get_cached_data("season_99_episodes").is_some());
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
