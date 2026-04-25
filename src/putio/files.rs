use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::warn;

use super::client::{ApiError, PutioClient};
use super::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};

const MAX_CONCURRENT: usize = 3;

#[derive(Debug, Deserialize)]
pub struct ListFilesResponse {
    #[serde(default)]
    pub files: Vec<PutIoFile>,
}

pub async fn list_folder(
    client: &PutioClient,
    token: &str,
    parent_id: u64,
) -> Result<ListFilesResponse, ApiError> {
    let url = format!(
        "https://api.put.io/v2/files/list?parent_id={parent_id}&stream_url_parent=true&mp4_stream_url_parent=true&mp4_status_parent=true&video_metadata_parent=true&codecs_parent=true&media_info_parent=true&breadcrumbs=true"
    );
    client.get_json(&url, Some(token)).await
}

pub async fn build_tree(
    client: PutioClient,
    token: String,
) -> Result<UnifiedDirectoryTree, ApiError> {
    let total_folders = Arc::new(AtomicU64::new(0));
    let total_files = Arc::new(AtomicU64::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let token = Arc::new(token);

    let root = build_node(
        client.clone(),
        token.clone(),
        None,
        0,
        sem,
        total_folders.clone(),
        total_files.clone(),
    )
    .await?;

    Ok(UnifiedDirectoryTree {
        root,
        last_refresh: Some(now_iso8601()),
        total_folders: total_folders.load(Ordering::Relaxed),
        total_files: total_files.load(Ordering::Relaxed),
    })
}

fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{secs}")
}

fn build_node<'a>(
    client: PutioClient,
    token: Arc<String>,
    self_file: Option<PutIoFile>,
    parent_id: u64,
    sem: Arc<Semaphore>,
    total_folders: Arc<AtomicU64>,
    total_files: Arc<AtomicU64>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<DirectoryNode, ApiError>> + Send + 'a>>
{
    Box::pin(async move {
        let permit = sem.clone().acquire_owned().await.expect("semaphore");
        let resp = list_folder(&client, &token, parent_id).await?;
        drop(permit);

        let mut folders = Vec::new();
        let mut files = Vec::new();
        for f in resp.files {
            if f.file_type == "FOLDER" {
                total_folders.fetch_add(1, Ordering::Relaxed);
                folders.push(f);
            } else {
                total_files.fetch_add(1, Ordering::Relaxed);
                files.push(f);
            }
        }

        let mut set: JoinSet<Result<(usize, DirectoryNode), ApiError>> = JoinSet::new();
        for (idx, folder) in folders.iter().enumerate() {
            let client = client.clone();
            let token = token.clone();
            let sem = sem.clone();
            let tf = total_folders.clone();
            let tfi = total_files.clone();
            let folder_clone = folder.clone();
            let folder_id = folder.id;
            set.spawn(async move {
                let node =
                    build_node(client, token, Some(folder_clone), folder_id, sem, tf, tfi).await?;
                Ok((idx, node))
            });
        }

        let mut children: Vec<Option<DirectoryNode>> = (0..folders.len()).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok((idx, node))) => {
                    children[idx] = Some(node);
                }
                Ok(Err(e)) => warn!("child fetch failed: {e}"),
                Err(e) => warn!("child task panic: {e}"),
            }
        }
        let children = children.into_iter().flatten().collect();

        Ok(DirectoryNode {
            file: self_file,
            children,
            files,
        })
    })
}
