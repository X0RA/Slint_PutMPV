use serde::Deserialize;

use super::{client::ApiError, types::PutIoFile, PutioClient};

#[derive(Clone, Debug, Deserialize)]
pub struct Subtitle {
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub language_code: String,
    #[serde(default)]
    pub source: String,
}

pub async fn list(
    client: &PutioClient,
    token: &str,
    file_id: u64,
) -> Result<Vec<Subtitle>, ApiError> {
    #[derive(Deserialize)]
    struct Response {
        subtitles: Vec<Subtitle>,
    }
    Ok(client
        .get_json::<Response>(
            &format!("https://api.put.io/v2/files/{file_id}/subtitles"),
            Some(token),
        )
        .await?
        .subtitles)
}

pub async fn download(
    client: &PutioClient,
    token: &str,
    file_id: u64,
    key: &str,
    limit: usize,
) -> Result<Vec<u8>, ApiError> {
    let mut url = reqwest::Url::parse(&format!("https://api.put.io/v2/files/{file_id}/subtitles/"))
        .expect("static API URL");
    url.path_segments_mut()
        .expect("hierarchical URL")
        .pop_if_empty()
        .push(key);
    url.query_pairs_mut().append_pair("format", "srt");
    client.get_bytes_limited(url.as_str(), token, limit).await
}

pub async fn languages(client: &PutioClient, token: &str) -> Result<Vec<String>, ApiError> {
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        subtitle_languages: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Response {
        settings: Settings,
    }
    Ok(client
        .get_json::<Response>("https://api.put.io/v2/account/settings", Some(token))
        .await?
        .settings
        .subtitle_languages)
}

pub async fn file(client: &PutioClient, token: &str, file_id: u64) -> Result<PutIoFile, ApiError> {
    #[derive(Deserialize)]
    struct Response {
        file: PutIoFile,
    }
    Ok(client
        .get_json::<Response>(
            &format!("https://api.put.io/v2/files/{file_id}"),
            Some(token),
        )
        .await?
        .file)
}
