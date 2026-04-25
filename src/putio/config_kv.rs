use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::client::{ApiError, PutioClient};

const CONFIG_BASE: &str = "https://api.put.io/v2/config";
pub const TMDB_KEY: &str = "tmdb_api_key";

#[derive(Debug, Deserialize)]
struct ConfigResponse {
    #[serde(default)]
    status: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    error_message: String,
}

#[derive(Debug, Serialize)]
struct PutBody<'a> {
    value: &'a str,
}

pub async fn get(client: &PutioClient, token: &str, key: &str) -> Result<String, ApiError> {
    let url = format!("{CONFIG_BASE}/{key}");
    match client.get_json::<ConfigResponse>(&url, Some(token)).await {
        Ok(resp) if resp.status == "OK" => Ok(resp.value),
        Ok(resp) => Err(ApiError::Http(StatusCode::BAD_GATEWAY, status_error(resp))),
        Err(ApiError::Http(StatusCode::NOT_FOUND, _)) => Ok(String::new()),
        Err(e) => Err(e),
    }
}

pub async fn put(
    client: &PutioClient,
    token: &str,
    key: &str,
    value: &str,
) -> Result<(), ApiError> {
    let url = format!("{CONFIG_BASE}/{key}");
    let resp = client
        .put_json::<_, ConfigResponse>(&url, token, &PutBody { value })
        .await?;
    if resp.status == "OK" {
        Ok(())
    } else {
        Err(ApiError::Http(StatusCode::BAD_GATEWAY, status_error(resp)))
    }
}

pub async fn delete(client: &PutioClient, token: &str, key: &str) -> Result<(), ApiError> {
    let url = format!("{CONFIG_BASE}/{key}");
    match client.delete_json::<ConfigResponse>(&url, token).await {
        Ok(resp) if resp.status == "OK" => Ok(()),
        Ok(resp) => Err(ApiError::Http(StatusCode::BAD_GATEWAY, status_error(resp))),
        Err(ApiError::Http(StatusCode::NOT_FOUND, _)) => Ok(()),
        Err(e) => Err(e),
    }
}

fn status_error(resp: ConfigResponse) -> String {
    if resp.error_message.is_empty() {
        format!("unexpected status {}", resp.status)
    } else {
        resp.error_message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ok_value_shape() {
        let resp: ConfigResponse =
            serde_json::from_str(r#"{"status":"OK","value":"abc"}"#).unwrap();
        assert_eq!(resp.status, "OK");
        assert_eq!(resp.value, "abc");
    }

    #[test]
    fn parses_error_shape() {
        let resp: ConfigResponse =
            serde_json::from_str(r#"{"status":"ERROR","error_message":"missing"}"#).unwrap();
        assert_eq!(status_error(resp), "missing");
    }
}
