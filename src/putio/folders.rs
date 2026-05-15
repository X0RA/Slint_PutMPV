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

pub async fn delete_trash_files(
    client: &PutioClient,
    token: &str,
    ids: &[u64],
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    let body = TrashDeleteBody {
        file_ids: ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(","),
    };
    let resp = client
        .post_json::<_, DeleteResponse>("https://api.put.io/v2/trash/delete", token, &body)
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
