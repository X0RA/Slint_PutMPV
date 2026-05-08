use serde::{Deserialize, Deserializer};

use super::client::{ApiError, PutioClient};
use super::types::PutIoFile;

fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn u64_or_default<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;
    struct U64OrDefault;
    impl<'de> de::Visitor<'de> for U64OrDefault {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an unsigned 64-bit integer or null")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            Ok(if v < 0 { 0 } else { v as u64 })
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<u64, E> {
            Ok(if v < 0.0 { 0 } else { v as u64 })
        }
        fn visit_none<E: de::Error>(self) -> Result<u64, E> {
            Ok(0)
        }
        fn visit_unit<E: de::Error>(self) -> Result<u64, E> {
            Ok(0)
        }
    }
    deserializer.deserialize_any(U64OrDefault)
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PutIoTransfer {
    #[serde(default, deserialize_with = "null_to_default")]
    pub current_ratio: f64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub downloaded: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub uploaded: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub down_speed: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub up_speed: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub error_message: String,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub estimated_time: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub file_id: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub finished_at: String,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub id: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub peers: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub percent_done: f64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub save_parent_id: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub seconds_seeding: u64,
    #[serde(default, deserialize_with = "u64_or_default")]
    pub size: u64,
    #[serde(default, deserialize_with = "null_to_default")]
    pub source: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(default, deserialize_with = "null_to_default")]
    pub tracker_message: String,
}

#[derive(Debug, Deserialize)]
struct TransferListResponse {
    #[serde(default)]
    transfers: Vec<PutIoTransfer>,
}

#[derive(Debug, Deserialize)]
struct TransferResponse {
    transfer: PutIoTransfer,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UploadTorrentResponse {
    Action { status: String, file: PutIoFile },
    File(PutIoFile),
}

pub async fn list(client: &PutioClient, token: &str) -> Result<Vec<PutIoTransfer>, ApiError> {
    let resp = client
        .get_json::<TransferListResponse>("https://api.put.io/v2/transfers/list", Some(token))
        .await?;
    Ok(resp.transfers)
}

pub async fn add_url(
    client: &PutioClient,
    token: &str,
    url: &str,
) -> Result<PutIoTransfer, ApiError> {
    let resp = client
        .post_form::<TransferResponse>(
            "https://api.put.io/v2/transfers/add",
            token,
            &[("url", url.to_string())],
        )
        .await?;
    Ok(resp.transfer)
}

pub async fn reannounce(client: &PutioClient, token: &str, id: u64) -> Result<(), ApiError> {
    client
        .post_form_no_body(
            "https://api.put.io/v2/transfers/reannounce",
            token,
            &[("id", id.to_string())],
        )
        .await
}

pub async fn retry(client: &PutioClient, token: &str, id: u64) -> Result<(), ApiError> {
    client
        .post_form_no_body(
            "https://api.put.io/v2/transfers/retry",
            token,
            &[("id", id.to_string())],
        )
        .await
}

pub async fn cancel(client: &PutioClient, token: &str, id: u64) -> Result<(), ApiError> {
    client
        .post_form_no_body(
            "https://api.put.io/v2/transfers/cancel",
            token,
            &[("transfer_ids", id.to_string())],
        )
        .await
}

pub async fn clean_completed(client: &PutioClient, token: &str) -> Result<(), ApiError> {
    client
        .post_form_no_body("https://api.put.io/v2/transfers/clean", token, &[])
        .await
}

pub async fn upload_torrent(
    client: &PutioClient,
    token: &str,
    filename: &str,
    body: Vec<u8>,
) -> Result<PutIoFile, ApiError> {
    let resp = client
        .upload_file_with_parent::<UploadTorrentResponse>(
            "https://upload.put.io/v2/files/upload",
            token,
            None,
            filename,
            body,
        )
        .await?;
    match resp {
        UploadTorrentResponse::Action { status, file } if status == "OK" => Ok(file),
        UploadTorrentResponse::Action { status, .. } => {
            Err(ApiError::Http(reqwest::StatusCode::BAD_GATEWAY, status))
        }
        UploadTorrentResponse::File(file) => Ok(file),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: put.io returns `null` for unset string/number fields on active
    // transfers (notably finished_at, error_message, tracker_message). This used
    // to fail with `invalid type: null, expected a string`.
    #[test]
    fn deserializes_transfer_with_null_fields() {
        let json = r#"{
            "id": 123,
            "name": "Active Download",
            "status": "DOWNLOADING",
            "percent_done": 42,
            "downloaded": 100,
            "size": 1000,
            "file_id": 0,
            "finished_at": null,
            "error_message": null,
            "tracker_message": null,
            "source": null,
            "estimated_time": null
        }"#;
        let t: PutIoTransfer = serde_json::from_str(json).expect("parses null fields");
        assert_eq!(t.id, 123);
        assert_eq!(t.name, "Active Download");
        assert_eq!(t.status, "DOWNLOADING");
        assert!(t.finished_at.is_empty());
        assert!(t.error_message.is_empty());
        assert!(t.tracker_message.is_empty());
        assert!(t.source.is_empty());
        assert_eq!(t.estimated_time, 0);
    }

    #[test]
    fn deserializes_transfer_with_negative_values() {
        let json = r#"{
            "id": 456,
            "name": "Seeding",
            "status": "SEEDING",
            "percent_done": 100.0,
            "downloaded": 500,
            "uploaded": 200,
            "size": 500,
            "file_id": 1,
            "current_ratio": 0.4,
            "estimated_time": -1,
            "peers": -1,
            "down_speed": -1,
            "up_speed": 102400,
            "seconds_seeding": -1,
            "finished_at": "2024-01-01T00:00:00"
        }"#;
        let t: PutIoTransfer = serde_json::from_str(json).expect("parses negative values");
        assert_eq!(t.id, 456);
        assert_eq!(t.status, "SEEDING");
        assert_eq!(t.estimated_time, 0);
        assert_eq!(t.peers, 0);
        assert_eq!(t.down_speed, 0);
        assert_eq!(t.up_speed, 102400);
        assert_eq!(t.seconds_seeding, 0);
    }

    #[test]
    fn deserializes_transfer_list_with_mixed_states() {
        let json = r#"{
            "transfers": [
                {"id": 1, "status": "DOWNLOADING", "name": "a", "finished_at": null, "tracker_message": null},
                {"id": 2, "status": "COMPLETED", "name": "b", "finished_at": "2024-01-01T00:00:00", "error_message": null}
            ]
        }"#;
        let resp: TransferListResponse = serde_json::from_str(json).expect("parses mixed list");
        assert_eq!(resp.transfers.len(), 2);
        assert_eq!(resp.transfers[0].status, "DOWNLOADING");
        assert_eq!(resp.transfers[1].finished_at, "2024-01-01T00:00:00");
    }
}
