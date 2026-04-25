use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{config_dir, read_json, write_atomic};
use crate::putio::types::UnifiedDirectoryTree;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FilesFile {
    #[serde(default)]
    pub tree: UnifiedDirectoryTree,
}

#[derive(Debug)]
pub struct FilesStore {
    path: PathBuf,
}

impl FilesStore {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("files.json");
        Ok(Self { path })
    }

    pub fn read_tree(&self) -> Result<UnifiedDirectoryTree> {
        match read_json::<FilesFile>(&self.path)? {
            Some(f) => Ok(f.tree),
            None => Ok(UnifiedDirectoryTree::default()),
        }
    }

    pub fn write_tree(&self, tree: &UnifiedDirectoryTree) -> Result<()> {
        let f = FilesFile { tree: tree.clone() };
        write_atomic(&self.path, &f)
    }

    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
}
