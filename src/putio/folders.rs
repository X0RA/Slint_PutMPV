use serde::{Deserialize, Serialize};

use super::client::{ApiError, PutioClient};
use super::files::list_folder;
use super::types::PutIoFile;

#[derive(Debug, Deserialize)]
struct FileActionResponse {
    status: String,
    file: PutIoFile,
}

#[derive(Debug, Deserialize)]
struct DownloadUrlResponse {
    url: String,
}

#[derive(Debug, Deserialize)]
struct DeleteResponse {
    status: String,
}

#[derive(Debug, Serialize)]
struct DeleteBody {
    file_ids: Vec<String>,
}

pub async fn find_or_create_folder(
    client: &PutioClient,
    token: &str,
    name: &str,
    parent_id: u64,
) -> Result<u64, ApiError> {
    let resp = list_folder(client, token, parent_id).await?;
    if let Some(folder) = resp
        .files
        .iter()
        .filter(|file| file.file_type == "FOLDER" && file.name == name)
        .max_by_key(|file| file.updated_at.clone())
    {
        return Ok(folder.id);
    }
    let created = create_folder(client, token, name, parent_id).await?;
    Ok(created.id)
}

pub async fn create_folder(
    client: &PutioClient,
    token: &str,
    name: &str,
    parent_id: u64,
) -> Result<PutIoFile, ApiError> {
    let resp = client
        .post_form::<FileActionResponse>(
            "https://api.put.io/v2/files/create-folder",
            token,
            &[
                ("name", name.to_string()),
                ("parent_id", parent_id.to_string()),
            ],
        )
        .await?;
    if resp.status == "OK" {
        Ok(resp.file)
    } else {
        Err(ApiError::Http(
            reqwest::StatusCode::BAD_GATEWAY,
            resp.status,
        ))
    }
}

pub async fn upload_file(
    client: &PutioClient,
    token: &str,
    parent_id: u64,
    filename: &str,
    body: Vec<u8>,
) -> Result<PutIoFile, ApiError> {
    let resp = client
        .upload_file::<FileActionResponse>(
            "https://upload.put.io/v2/files/upload",
            token,
            parent_id,
            filename,
            body,
        )
        .await?;
    if resp.status == "OK" {
        Ok(resp.file)
    } else {
        Err(ApiError::Http(
            reqwest::StatusCode::BAD_GATEWAY,
            resp.status,
        ))
    }
}

pub async fn download_file(
    client: &PutioClient,
    token: &str,
    file_id: u64,
) -> Result<Vec<u8>, ApiError> {
    let url = format!("https://api.put.io/v2/files/{file_id}/url");
    let resp = client
        .get_json::<DownloadUrlResponse>(&url, Some(token))
        .await?;
    client.get_bytes(&resp.url, None).await
}

pub async fn rename_file(
    client: &PutioClient,
    token: &str,
    file_id: u64,
    name: &str,
) -> Result<(), ApiError> {
    let resp = client
        .post_form::<DeleteResponse>(
            "https://api.put.io/v2/files/rename",
            token,
            &[("file_id", file_id.to_string()), ("name", name.to_string())],
        )
        .await?;
    if resp.status == "OK" {
        Ok(())
    } else {
        Err(ApiError::Http(
            reqwest::StatusCode::BAD_GATEWAY,
            resp.status,
        ))
    }
}

#[derive(Debug, Serialize)]
struct TrashDeleteBody {
    file_ids: String,
}

#[derive(Debug, Deserialize)]
struct TrashListResponse {
    #[serde(default)]
    files: Vec<PutIoFile>,
    #[serde(default)]
    cursor: Option<String>,
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn trash_file_ids_body(ids: &[u64]) -> TrashDeleteBody {
    TrashDeleteBody {
        file_ids: ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(","),
    }
}

async fn trash_post(
    client: &PutioClient,
    token: &str,
    url: &str,
    ids: &[u64],
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    let resp = client
        .post_json::<_, DeleteResponse>(url, token, &trash_file_ids_body(ids))
        .await?;
    if resp.status == "OK" {
        Ok(())
    } else {
        Err(ApiError::Http(
            reqwest::StatusCode::BAD_GATEWAY,
            resp.status,
        ))
    }
}

pub async fn list_trash(client: &PutioClient, token: &str) -> Result<Vec<PutIoFile>, ApiError> {
    let mut files = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match cursor.as_deref() {
            Some(cursor) if !cursor.is_empty() => {
                format!(
                    "https://api.put.io/v2/trash/list?per_page=50&cursor={}",
                    encode_query_value(cursor)
                )
            }
            _ => "https://api.put.io/v2/trash/list?per_page=50".to_string(),
        };
        let resp = client
            .get_json::<TrashListResponse>(&url, Some(token))
            .await?;
        files.extend(resp.files);
        match resp.cursor.filter(|c| !c.is_empty()) {
            // A cursor that does not advance would otherwise loop forever.
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => break,
        }
    }
    Ok(files)
}

pub async fn restore_trash_files(
    client: &PutioClient,
    token: &str,
    ids: &[u64],
) -> Result<(), ApiError> {
    trash_post(client, token, "https://api.put.io/v2/trash/restore", ids).await
}

pub async fn delete_trash_files(
    client: &PutioClient,
    token: &str,
    ids: &[u64],
) -> Result<(), ApiError> {
    trash_post(client, token, "https://api.put.io/v2/trash/delete", ids).await
}

pub async fn delete_files(client: &PutioClient, token: &str, ids: &[u64]) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    let body = DeleteBody {
        file_ids: ids.iter().map(|id| id.to_string()).collect(),
    };
    let resp = client
        .post_json::<_, DeleteResponse>(
            "https://api.put.io/v2/files/delete?skip_nonexistents=true&skip_owner_check=false&partial_delete=true",
            token,
            &body,
        )
        .await?;
    if resp.status == "OK" {
        Ok(())
    } else {
        Err(ApiError::Http(
            reqwest::StatusCode::BAD_GATEWAY,
            resp.status,
        ))
    }
}
