use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, read_json, write_atomic};

/// IMDb rating for one title. `rating` is `None` when IMDb has no votes for it.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ImdbRating {
    #[serde(default)]
    pub rating: Option<f64>,
    #[serde(default)]
    pub votes: Option<u64>,
    #[serde(default)]
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ImdbCache {
    #[serde(default)]
    pub ratings: HashMap<String, ImdbRating>,
}

#[derive(Debug)]
pub struct ImdbStore {
    path: PathBuf,
    access: Mutex<()>,
}

impl ImdbStore {
    #[cfg(test)]
    pub(crate) fn at_path(path: PathBuf) -> Self {
        Self {
            path,
            access: Mutex::new(()),
        }
    }

    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("imdb.json");
        if read_json::<ImdbCache>(&path)?.is_none() {
            write_atomic(&path, &ImdbCache::default())?;
        }
        Ok(Self {
            path,
            access: Mutex::new(()),
        })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn get_cache_snapshot(&self) -> Result<ImdbCache> {
        let _access = self.access.lock().unwrap();
        self.read_cache()
    }

    pub fn set_ratings(&self, ratings: HashMap<String, ImdbRating>) -> Result<()> {
        if ratings.is_empty() {
            return Ok(());
        }
        let _access = self.access.lock().unwrap();
        let mut cache = self.read_cache()?;
        cache.ratings.extend(ratings);
        self.save_cache(&cache)
    }

    pub fn clear_cache(&self) -> Result<()> {
        let _access = self.access.lock().unwrap();
        self.save_cache(&ImdbCache::default())
    }

    fn read_cache(&self) -> Result<ImdbCache> {
        Ok(read_json::<ImdbCache>(&self.path)?.unwrap_or_default())
    }

    fn save_cache(&self, cache: &ImdbCache) -> Result<()> {
        write_atomic(&self.path, cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ratings_merges_with_existing_entries() {
        let dir = std::env::temp_dir().join(format!("putmpv_imdb_store_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ImdbStore::at_path(dir.join("imdb.json"));
        let rating = |r: f64| ImdbRating {
            rating: Some(r),
            votes: Some(10),
            fetched_at: 1,
        };
        store
            .set_ratings(HashMap::from([("tt1".to_string(), rating(7.0))]))
            .unwrap();
        store
            .set_ratings(HashMap::from([("tt2".to_string(), rating(8.0))]))
            .unwrap();
        let cache = store.get_cache_snapshot().unwrap();
        assert_eq!(cache.ratings["tt1"].rating, Some(7.0));
        assert_eq!(cache.ratings["tt2"].rating, Some(8.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
