use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PutIoFile {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub file_type: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub parent_id: Option<u64>,
    #[serde(default)]
    pub folder_type: Option<String>,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub is_mp4_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DirectoryNode {
    #[serde(default)]
    pub file: Option<PutIoFile>,
    #[serde(default)]
    pub children: Vec<DirectoryNode>,
    #[serde(default)]
    pub files: Vec<PutIoFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnifiedDirectoryTree {
    #[serde(default)]
    pub root: DirectoryNode,
    #[serde(default)]
    pub last_refresh: Option<String>,
    #[serde(default)]
    pub total_folders: u64,
    #[serde(default)]
    pub total_files: u64,
}
