use serde::Deserialize;

use super::client::{ApiError, PutioClient};

#[derive(Debug, Deserialize)]
struct OobCodeResponse {
    code: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    oauth_token: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Debug)]
pub enum PollResult {
    Pending,
    Token(String),
}

pub async fn get_device_code(client: &PutioClient, app_id: u32) -> Result<String, ApiError> {
    let url = format!("https://api.put.io/v2/oauth2/oob/code?app_id={app_id}");
    let resp: OobCodeResponse = client.get_json(&url, None).await?;
    Ok(resp.code)
}

pub async fn poll_token(client: &PutioClient, code: &str) -> Result<PollResult, ApiError> {
    let url = format!("https://api.put.io/v2/oauth2/oob/code/{code}");
    let resp: TokenResponse = client.get_json(&url, None).await?;
    match resp.status.as_str() {
        "OK" => match resp.oauth_token {
            Some(t) if !t.is_empty() => Ok(PollResult::Token(t)),
            _ => Ok(PollResult::Pending),
        },
        "ERROR" => {
            let msg = resp
                .error_message
                .unwrap_or_else(|| "unknown put.io error".into());
            Err(ApiError::Http(reqwest::StatusCode::BAD_REQUEST, msg))
        }
        _ => Ok(PollResult::Pending),
    }
}

pub async fn check_token_validity(client: &PutioClient, token: &str) -> Result<bool, ApiError> {
    let url = "https://api.put.io/v2/account/info";
    match client.get_json::<serde_json::Value>(url, Some(token)).await {
        Ok(_) => Ok(true),
        Err(ApiError::Unauthorized) => Ok(false),
        Err(e) => Err(e),
    }
}
