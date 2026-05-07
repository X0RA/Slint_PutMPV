use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, write_atomic};

const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct FileStateFile {
    pub version: u32,
    pub entries: BTreeMap<String, FileStateEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Unwatched,
    Partial,
    Watched,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub struct FileStateEntry {
    #[serde(default)]
    pub position_secs: f64,
    #[serde(default)]
    pub duration_secs: f64,
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub updated_at: i64,
}

impl FileStateEntry {
    pub fn is_completed(&self) -> bool {
        self.played
            || (self.duration_secs > 0.0
                && (self.position_secs / self.duration_secs).clamp(0.0, 1.0) >= 0.9)
    }

    pub fn progress_ratio(&self) -> f32 {
        if self.played {
            1.0
        } else if self.duration_secs > 0.0 {
            (self.position_secs / self.duration_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }

    pub fn watch_state(&self) -> WatchState {
        if self.is_completed() {
            WatchState::Watched
        } else if self.progress_ratio() >= 0.05 {
            WatchState::Partial
        } else {
            WatchState::Unwatched
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileStateStore {
    path: PathBuf,
    entries: BTreeMap<String, FileStateEntry>,
}

impl FileStateStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("file_state.json");
        let entries = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => BTreeMap::new(),
            Ok(bytes) => serde_json::from_slice::<FileStateFile>(&bytes)
                .ok()
                .filter(|file| file.version == STORE_VERSION)
                .map(|file| file.entries)
                .unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, entries })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn entries(&self) -> &BTreeMap<String, FileStateEntry> {
        &self.entries
    }

    pub fn set_watched(&mut self, id: u64, watched: bool) {
        let key = id.to_string();
        let mut entry = self.entries.get(&key).copied().unwrap_or_default();
        entry.played = watched;
        if !watched {
            entry.position_secs = 0.0;
        }
        entry.updated_at = now_unix();
        self.entries.insert(key, entry);
    }

    pub fn update_position(&mut self, id: u64, position_secs: f64, duration_secs: f64) {
        let key = id.to_string();
        let mut entry = self.entries.get(&key).copied().unwrap_or_default();
        entry.position_secs = finite_nonnegative(position_secs);
        if duration_secs.is_finite() && duration_secs > 0.0 {
            entry.duration_secs = duration_secs;
        }
        entry.updated_at = now_unix();
        self.entries.insert(key, entry);
    }

    pub fn clear_played(&mut self) -> bool {
        let mut changed = false;
        let updated_at = now_unix();
        for entry in self.entries.values_mut() {
            if entry.played || entry.position_secs > 0.0 || entry.duration_secs > 0.0 {
                entry.played = false;
                entry.position_secs = 0.0;
                entry.duration_secs = 0.0;
                entry.updated_at = updated_at;
                changed = true;
            }
        }
        changed
    }

    pub fn merge(&mut self, remote: &BTreeMap<String, FileStateEntry>) -> bool {
        let before = self.entries.clone();
        let keys = self
            .entries
            .keys()
            .chain(remote.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut merged = BTreeMap::new();
        for key in keys {
            match (self.entries.get(&key), remote.get(&key)) {
                (Some(local), Some(remote)) => {
                    merged.insert(
                        key,
                        if remote.updated_at >= local.updated_at {
                            *remote
                        } else {
                            *local
                        },
                    );
                }
                (Some(local), None) => {
                    merged.insert(key, *local);
                }
                (None, Some(remote)) => {
                    merged.insert(key, *remote);
                }
                (None, None) => {}
            }
        }
        let changed = before != merged;
        self.entries = merged;
        changed
    }

    #[allow(dead_code)]
    pub fn replace(&mut self, entries: BTreeMap<String, FileStateEntry>) {
        self.entries = entries;
    }

    pub fn save(&self) -> Result<()> {
        write_atomic(
            &self.path,
            &FileStateFile {
                version: STORE_VERSION,
                entries: self.entries.clone(),
            },
        )
    }
}

pub fn count_completed(entries: &BTreeMap<String, FileStateEntry>) -> usize {
    entries
        .values()
        .filter(|entry| matches!(entry.watch_state(), WatchState::Watched))
        .count()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prefers_newer_entries() {
        let mut store = FileStateStore {
            path: PathBuf::from("unused"),
            entries: BTreeMap::from([
                (
                    "1".to_string(),
                    FileStateEntry {
                        position_secs: 10.0,
                        duration_secs: 100.0,
                        played: true,
                        updated_at: 10,
                    },
                ),
                (
                    "2".to_string(),
                    FileStateEntry {
                        position_secs: 20.0,
                        duration_secs: 100.0,
                        played: true,
                        updated_at: 30,
                    },
                ),
            ]),
        };
        let remote = BTreeMap::from([
            (
                "1".to_string(),
                FileStateEntry {
                    position_secs: 40.0,
                    duration_secs: 100.0,
                    played: false,
                    updated_at: 20,
                },
            ),
            (
                "3".to_string(),
                FileStateEntry {
                    position_secs: 90.0,
                    duration_secs: 100.0,
                    played: true,
                    updated_at: 40,
                },
            ),
        ]);
        assert!(store.merge(&remote));
        assert!(!store.entries["1"].played);
        assert!(store.entries["2"].played);
        assert!(store.entries["3"].played);
    }

    #[test]
    fn clear_played_preserves_entries() {
        let mut store = FileStateStore {
            path: PathBuf::from("unused"),
            entries: BTreeMap::from([
                (
                    "1".to_string(),
                    FileStateEntry {
                        position_secs: 10.0,
                        duration_secs: 100.0,
                        played: true,
                        updated_at: 10,
                    },
                ),
                (
                    "2".to_string(),
                    FileStateEntry {
                        position_secs: 20.0,
                        duration_secs: 100.0,
                        played: false,
                        updated_at: 30,
                    },
                ),
            ]),
        };
        assert!(store.clear_played());
        assert_eq!(store.entries.len(), 2);
        assert!(!store.entries.values().any(|entry| entry.played));
        assert!(!store
            .entries
            .values()
            .any(|entry| entry.position_secs > 0.0));
    }

    #[test]
    fn completion_and_progress_are_derived() {
        let unwatched = FileStateEntry::default();
        assert_eq!(unwatched.watch_state(), WatchState::Unwatched);
        assert_eq!(unwatched.progress_ratio(), 0.0);

        let partial = FileStateEntry {
            position_secs: 89.0,
            duration_secs: 100.0,
            played: false,
            updated_at: 1,
        };
        assert!(!partial.is_completed());
        assert_eq!(partial.watch_state(), WatchState::Partial);

        let completed = FileStateEntry {
            position_secs: 90.0,
            duration_secs: 100.0,
            played: false,
            updated_at: 1,
        };
        assert!(completed.is_completed());
        assert_eq!(completed.watch_state(), WatchState::Watched);

        let explicit = FileStateEntry {
            played: true,
            ..Default::default()
        };
        assert_eq!(explicit.progress_ratio(), 1.0);
        assert!(explicit.is_completed());
    }
}
