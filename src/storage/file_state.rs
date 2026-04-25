use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, read_json, write_atomic};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FileStateEntry {
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug)]
pub struct FileStateStore {
    path: PathBuf,
    entries: BTreeMap<String, FileStateEntry>,
}

impl FileStateStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("file_state.json");
        let entries = read_json::<BTreeMap<String, FileStateEntry>>(&path)?.unwrap_or_default();
        Ok(Self { path, entries })
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn entries(&self) -> &BTreeMap<String, FileStateEntry> {
        &self.entries
    }

    #[allow(dead_code)]
    pub fn set_played(&mut self, id: u64, played: bool) {
        let key = id.to_string();
        let mut entry = self.entries.get(&key).copied().unwrap_or_default();
        entry.played = played;
        entry.updated_at = now_unix();
        self.entries.insert(key, entry);
    }

    pub fn clear_played(&mut self) -> bool {
        let mut changed = false;
        let updated_at = now_unix();
        for entry in self.entries.values_mut() {
            if entry.played {
                entry.played = false;
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
        write_atomic(&self.path, &self.entries)
    }
}

pub fn count_played(entries: &BTreeMap<String, FileStateEntry>) -> usize {
    entries.values().filter(|entry| entry.played).count()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
                        played: true,
                        updated_at: 10,
                    },
                ),
                (
                    "2".to_string(),
                    FileStateEntry {
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
                    played: false,
                    updated_at: 20,
                },
            ),
            (
                "3".to_string(),
                FileStateEntry {
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
                        played: true,
                        updated_at: 10,
                    },
                ),
                (
                    "2".to_string(),
                    FileStateEntry {
                        played: false,
                        updated_at: 30,
                    },
                ),
            ]),
        };
        assert!(store.clear_played());
        assert_eq!(store.entries.len(), 2);
        assert!(!store.entries.values().any(|entry| entry.played));
    }
}
