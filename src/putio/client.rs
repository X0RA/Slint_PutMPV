use std::time::Duration;

use reqwest::{multipart, Client, StatusCode};
use serde::Serialize;
use thiserror::Error;

const USER_AGENT: &str = "PutMPV/1.0";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("http {0}: {1}")]
    Http(StatusCode, String),
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct PutioClient {
    http: Client,
}

impl Default for PutioClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PutioClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client build");
        Self { http }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: Option<&str>,
    ) -> Result<T, ApiError> {
        let mut req = self.http.get(url);
        if let Some(t) = token {
            req = req.header("Authorization", format!("token {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let s = String::from_utf8_lossy(&body).into_owned();
            return Err(ApiError::Http(status, s));
        }
        let parsed = serde_json::from_slice::<T>(&body)?;
        Ok(parsed)
    }

    pub async fn get_bytes(&self, url: &str, token: Option<&str>) -> Result<Vec<u8>, ApiError> {
        let mut req = self.http.get(url);
        if let Some(t) = token {
            req = req.header("Authorization", format!("token {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let s = String::from_utf8_lossy(&body).into_owned();
            return Err(ApiError::Http(status, s));
        }
        Ok(body.to_vec())
    }

    pub async fn put_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        self.send_json(self.http.put(url), token, body).await
    }

    pub async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        self.send_json(self.http.post(url), token, body).await
    }

    pub async fn delete_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
    ) -> Result<T, ApiError> {
        let req = self
            .http
            .delete(url)
            .header("Authorization", format!("token {token}"));
        self.parse_response(req.send().await?).await
    }

    pub async fn post_form<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
        form: &[(&str, String)],
    ) -> Result<T, ApiError> {
        let req = self
            .http
            .post(url)
            .header("Authorization", format!("token {token}"))
            .form(form);
        self.parse_response(req.send().await?).await
    }

    pub async fn post_form_no_body(
        &self,
        url: &str,
        token: &str,
        form: &[(&str, String)],
    ) -> Result<(), ApiError> {
        let req = self
            .http
            .post(url)
            .header("Authorization", format!("token {token}"))
            .form(form);
        self.parse_empty_response(req.send().await?).await
    }

    pub async fn upload_file<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
        parent_id: u64,
        filename: &str,
        body: Vec<u8>,
    ) -> Result<T, ApiError> {
        self.upload_file_with_parent(url, token, Some(parent_id), filename, body)
            .await
    }

    pub async fn upload_file_with_parent<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
        parent_id: Option<u64>,
        filename: &str,
        body: Vec<u8>,
    ) -> Result<T, ApiError> {
        let part = multipart::Part::bytes(body).file_name(filename.to_string());
        let form = multipart::Form::new()
            .part("file", part)
            .text("filename", filename.to_string());
        let form = if let Some(parent_id) = parent_id {
            form.text("parent_id", parent_id.to_string())
        } else {
            form
        };
        let req = self
            .http
            .post(url)
            .header("Authorization", format!("token {token}"))
            .multipart(form);
        self.parse_response(req.send().await?).await
    }

    async fn send_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        token: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = req
            .header("Authorization", format!("token {token}"))
            .json(body);
        self.parse_response(req.send().await?).await
    }

    async fn parse_response<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = resp.status();
        let body = resp.bytes().await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let s = String::from_utf8_lossy(&body).into_owned();
            return Err(ApiError::Http(status, s));
        }
        Ok(serde_json::from_slice::<T>(&body)?)
    }

    async fn parse_empty_response(&self, resp: reqwest::Response) -> Result<(), ApiError> {
        let status = resp.status();
        let body = resp.bytes().await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            let s = String::from_utf8_lossy(&body).into_owned();
            return Err(ApiError::Http(status, s));
        }
        Ok(())
    }
}
