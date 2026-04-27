use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, read_json, write_atomic};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MatchedData {
    #[serde(default)]
    pub movies: HashMap<String, i32>,
    #[serde(default)]
    pub tv: HashMap<String, i32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tv_source: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub movie_scraped_at: HashMap<String, i64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tv_scraped_at: HashMap<String, i64>,
}

#[derive(Debug)]
pub struct MatchedStore {
    path: PathBuf,
}

impl MatchedStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("matched.json");
        if read_json::<MatchedData>(&path)?.is_none() {
            write_atomic(&path, &MatchedData::default())?;
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn insert_movie(&self, file_id: &str, movie_id: i32) -> Result<()> {
        let mut data = self.read_data()?;
        data.movies.insert(file_id.to_string(), movie_id);
        data.movie_scraped_at
            .insert(file_id.to_string(), now_unix_millis());
        self.save_data(&data)
    }

    pub fn insert_tv_with_source(&self, file_id: &str, episode_id: i32, source: &str) -> Result<()> {
        let mut data = self.read_data()?;
        data.tv.insert(file_id.to_string(), episode_id);
        data.tv_source.insert(file_id.to_string(), source.to_string());
        data.tv_scraped_at
            .insert(file_id.to_string(), now_unix_millis());
        self.save_data(&data)
    }

    pub fn delete_movie(&self, file_id: &str) -> Result<()> {
        let mut data = self.read_data()?;
        data.movies.remove(file_id);
        data.movie_scraped_at.remove(file_id);
        self.save_data(&data)
    }

    pub fn delete_tv(&self, file_id: &str) -> Result<()> {
        let mut data = self.read_data()?;
        data.tv.remove(file_id);
        data.tv_source.remove(file_id);
        data.tv_scraped_at.remove(file_id);
        self.save_data(&data)
    }

    pub fn get_matched_snapshot(&self) -> Result<MatchedData> {
        self.read_data()
    }

    pub fn clear(&self) -> Result<()> {
        self.save_data(&MatchedData::default())
    }

    fn read_data(&self) -> Result<MatchedData> {
        Ok(read_json::<MatchedData>(&self.path)?.unwrap_or_default())
    }

    fn save_data(&self, data: &MatchedData) -> Result<()> {
        write_atomic(&self.path, data)
    }
}

fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_delete_matches() {
        let dir = tempfile_path("matched_store_insert");
        let store = MatchedStore {
            path: dir.join("matched.json"),
        };

        store.insert_movie("10", 100).unwrap();
        store.insert_tv_with_source("20", 200, "tvmaze").unwrap();
        let data = store.get_matched_snapshot().unwrap();
        assert_eq!(data.movies["10"], 100);
        assert_eq!(data.tv["20"], 200);
        assert_eq!(data.tv_source["20"], "tvmaze");
        assert!(data.movie_scraped_at["10"] > 0);
        assert!(data.tv_scraped_at["20"] > 0);

        store.delete_movie("10").unwrap();
        store.delete_tv("20").unwrap();
        let data = store.get_matched_snapshot().unwrap();
        assert!(data.movies.is_empty());
        assert!(data.tv.is_empty());
        assert!(data.tv_source.is_empty());
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
