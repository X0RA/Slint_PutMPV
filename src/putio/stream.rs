use serde::Deserialize;

use super::client::{ApiError, PutioClient};

#[derive(Debug, Deserialize)]
struct DownloadUrlResponse {
    url: String,
}

pub async fn resolve_play_url(
    client: &PutioClient,
    token: &str,
    file_id: u64,
) -> Result<String, ApiError> {
    let url = format!("https://api.put.io/v2/files/{file_id}/url");
    let resp = client
        .get_json::<DownloadUrlResponse>(&url, Some(token))
        .await?;
    Ok(resp.url)
}

pub fn fallback_mp4_stream_url(token: &str, file_id: u64) -> String {
    format!("https://api.put.io/v2/files/{file_id}/mp4/stream?oauth_token={token}")
}
