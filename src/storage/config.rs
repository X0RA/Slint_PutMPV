use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, read_json, write_atomic};

pub const DEFAULT_PUT_CLIENT_ID: u32 = 8275;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default = "default_client_id")]
    pub put_client_id: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub oauth_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tmdb_api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tmdb_api_key_putio: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tmdb_api_key_source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_state_sync_profile_slug: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_state_sync_profile_name: String,
    #[serde(default)]
    pub files_mode: i32,
    #[serde(default)]
    pub files_sort: i32,
    #[serde(default = "default_files_sort_descending")]
    pub files_sort_descending: bool,
}

fn default_client_id() -> u32 {
    DEFAULT_PUT_CLIENT_ID
}

fn default_files_sort_descending() -> bool {
    true
}

#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    inner: Mutex<ConfigFile>,
}

impl ConfigStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("config.json");
        let cfg: ConfigFile = match read_json::<ConfigFile>(&path)? {
            Some(mut c) => {
                if c.put_client_id == 0 {
                    c.put_client_id = DEFAULT_PUT_CLIENT_ID;
                }
                c
            }
            None => {
                let c = ConfigFile {
                    put_client_id: DEFAULT_PUT_CLIENT_ID,
                    oauth_token: String::new(),
                    tmdb_api_key: String::new(),
                    tmdb_api_key_putio: String::new(),
                    tmdb_api_key_source: String::new(),
                    file_state_sync_profile_slug: String::new(),
                    file_state_sync_profile_name: String::new(),
                    files_mode: 0,
                    files_sort: 0,
                    files_sort_descending: true,
                };
                write_atomic(&path, &c)?;
                c
            }
        };
        Ok(Self {
            path,
            inner: Mutex::new(cfg),
        })
    }

    pub fn oauth_token(&self) -> String {
        self.inner.lock().unwrap().oauth_token.clone()
    }

    pub fn put_client_id(&self) -> u32 {
        self.inner.lock().unwrap().put_client_id
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn set_oauth_token(&self, token: &str) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.oauth_token = token.to_string();
        write_atomic(&self.path, &*guard)
    }

    pub fn clear_oauth_token(&self) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        if guard.oauth_token.is_empty() {
            return Ok(());
        }
        guard.oauth_token.clear();
        write_atomic(&self.path, &*guard)
    }

    pub fn tmdb_local_key(&self) -> String {
        self.inner.lock().unwrap().tmdb_api_key.clone()
    }

    pub fn set_tmdb_local_key(&self, key: &str) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.tmdb_api_key = key.to_string();
        write_atomic(&self.path, &*guard)
    }

    pub fn tmdb_putio_key(&self) -> String {
        self.inner.lock().unwrap().tmdb_api_key_putio.clone()
    }

    pub fn set_tmdb_putio_key(&self, key: &str) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.tmdb_api_key_putio = key.to_string();
        write_atomic(&self.path, &*guard)
    }

    pub fn tmdb_source(&self) -> String {
        let source = self.inner.lock().unwrap().tmdb_api_key_source.clone();
        if source == "putio" {
            source
        } else {
            "local".to_string()
        }
    }

    pub fn set_tmdb_source(&self, source: &str) -> Result<()> {
        let source = if source == "putio" { "putio" } else { "local" };
        let mut guard = self.inner.lock().unwrap();
        guard.tmdb_api_key_source = source.to_string();
        write_atomic(&self.path, &*guard)
    }

    pub fn file_state_sync_profile(&self) -> (String, String) {
        let guard = self.inner.lock().unwrap();
        (
            guard.file_state_sync_profile_slug.clone(),
            guard.file_state_sync_profile_name.clone(),
        )
    }

    pub fn set_file_state_sync_profile(&self, slug: &str, name: &str) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.file_state_sync_profile_slug = slug.to_string();
        guard.file_state_sync_profile_name = name.to_string();
        write_atomic(&self.path, &*guard)
    }

    pub fn clear_file_state_sync_profile(&self) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.file_state_sync_profile_slug.clear();
        guard.file_state_sync_profile_name.clear();
        write_atomic(&self.path, &*guard)
    }

    pub fn files_mode(&self) -> i32 {
        match self.inner.lock().unwrap().files_mode {
            1 => 1,
            _ => 0,
        }
    }

    pub fn set_files_mode(&self, mode: i32) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.files_mode = if mode == 1 { 1 } else { 0 };
        write_atomic(&self.path, &*guard)
    }

    pub fn files_sort(&self) -> i32 {
        let sort = self.inner.lock().unwrap().files_sort;
        match sort {
            1..=3 => sort,
            _ => 0,
        }
    }

    pub fn set_files_sort(&self, sort: i32) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.files_sort = if (1..=3).contains(&sort) { sort } else { 0 };
        write_atomic(&self.path, &*guard)
    }

    pub fn files_sort_descending(&self) -> bool {
        self.inner.lock().unwrap().files_sort_descending
    }

    pub fn set_files_sort_descending(&self, descending: bool) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        guard.files_sort_descending = descending;
        write_atomic(&self.path, &*guard)
    }

    pub fn reset_to_defaults(&self) -> Result<()> {
        let mut guard = self.inner.lock().unwrap();
        *guard = ConfigFile {
            put_client_id: DEFAULT_PUT_CLIENT_ID,
            files_sort_descending: true,
            ..ConfigFile::default()
        };
        write_atomic(&self.path, &*guard)
    }
}
