use std::collections::BTreeMap;

use crate::storage::config::ConfigStore;
use crate::storage::file_state::{count_completed, FileStateEntry, FileStateFile, FileStateStore};
use anyhow::{anyhow, Result};
use tracing::warn;

use super::client::PutioClient;
use super::files::list_folder;
use super::folders::{
    delete_files, delete_trash_files, download_file, find_or_create_folder, rename_file,
    upload_file,
};
use super::types::PutIoFile;

const REMOTE_FOLDER: &str = "PutMPV";
const PROFILE_SUFFIX: &str = "_file_state.json";

#[derive(Debug, Clone)]
pub struct SyncProfile {
    pub slug: String,
    pub name: String,
    pub total_played: usize,
}

pub async fn list_profiles(
    client: &PutioClient,
    token: &str,
    cfg: &ConfigStore,
) -> Result<Vec<SyncProfile>> {
    let root = list_folder(client, token, 0).await?;
    let Some(folder) = root
        .files
        .iter()
        .filter(|file| file.file_type == "FOLDER" && file.name == REMOTE_FOLDER)
        .max_by_key(|file| file.updated_at.clone())
    else {
        return Ok(Vec::new());
    };
    let files = list_folder(client, token, folder.id).await?.files;
    let mut profiles = Vec::new();
    for file in latest_profile_files(files).into_values() {
        let Some(slug) = parse_profile_slug(&file.name) else {
            continue;
        };
        let state = download_state(client, token, file.id)
            .await
            .unwrap_or_default();
        profiles.push(SyncProfile {
            name: profile_name_from_slug(cfg, &slug),
            slug,
            total_played: count_completed(&state),
        });
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name).then(a.slug.cmp(&b.slug)));
    Ok(profiles)
}

pub async fn select_profile(
    client: &PutioClient,
    token: &str,
    cfg: &ConfigStore,
    store: &mut FileStateStore,
    name: &str,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("profile name cannot be empty"));
    }
    let slug = slugify(name)?;
    sync_profile(client, token, store, &slug).await?;
    cfg.set_file_state_sync_profile(&slug, name)?;
    Ok(slug)
}

pub fn disable_sync(cfg: &ConfigStore) -> Result<()> {
    cfg.clear_file_state_sync_profile()
}

pub async fn sync_profile(
    client: &PutioClient,
    token: &str,
    store: &mut FileStateStore,
    slug: &str,
) -> Result<()> {
    let folder_id = find_or_create_folder(client, token, REMOTE_FOLDER, 0).await?;
    let profile_file = find_remote_profile(client, token, folder_id, slug).await?;
    let remote = match &profile_file {
        Some(file) => download_state(client, token, file.id).await?,
        None => BTreeMap::new(),
    };
    store.merge(&remote);
    store.save()?;
    let body = serde_json::to_vec_pretty(&FileStateFile {
        version: 1,
        entries: store.entries().clone(),
    })?;
    let canonical = profile_filename(slug);
    let pre_upload_boundary = profile_file.as_ref().and_then(|f| f.updated_at.clone());
    let uploaded = upload_file(client, token, folder_id, &canonical, body).await?;
    let mut trashed: Vec<u64> = Vec::new();
    if let Some(old) = profile_file {
        if old.id != uploaded.id {
            trashed = delete_profile_duplicates(
                client,
                token,
                folder_id,
                slug,
                uploaded.id,
                pre_upload_boundary.as_deref(),
            )
            .await?;
        }
    }
    if uploaded.name != canonical {
        if let Err(e) = rename_file(client, token, uploaded.id, &canonical).await {
            // Cosmetic; the file is uploaded and indexable by slug regardless.
            warn!("could not rename uploaded profile to canonical name: {e}");
        }
    }
    if !trashed.is_empty() {
        delete_trash_files(client, token, &trashed)
            .await
            .map_err(|e| anyhow!("could not purge profile files from put.io trash: {e}"))?;
    }
    Ok(())
}

async fn find_remote_profile(
    client: &PutioClient,
    token: &str,
    folder_id: u64,
    slug: &str,
) -> Result<Option<PutIoFile>> {
    Ok(list_folder(client, token, folder_id)
        .await?
        .files
        .into_iter()
        .filter(|file| {
            file.file_type != "FOLDER" && parse_profile_slug(&file.name).as_deref() == Some(slug)
        })
        .max_by_key(|file| file.updated_at.clone()))
}

async fn delete_profile_duplicates(
    client: &PutioClient,
    token: &str,
    folder_id: u64,
    slug: &str,
    keep_file_id: u64,
    // ISO8601 updated_at of the profile file we downloaded at the start of
    // this sync. Files newer than this were uploaded by another device after
    // we began and must not be trashed — the next sync will merge their
    // state in. If None, we never saw a prior profile (first sync), so the
    // boundary is "before our upload" — equivalent to "any file other than
    // the one we just uploaded".
    pre_upload_boundary: Option<&str>,
) -> Result<Vec<u64>> {
    let filename = profile_filename(slug);
    let ids = list_folder(client, token, folder_id)
        .await?
        .files
        .into_iter()
        .filter(|file| {
            file.file_type != "FOLDER"
                && (file.name == filename
                    || parse_profile_slug(&file.name).as_deref() == Some(slug))
                && file.id != keep_file_id
                && match (pre_upload_boundary, file.updated_at.as_deref()) {
                    (Some(boundary), Some(updated)) => updated <= boundary,
                    // If we don't know the boundary or the file lacks a
                    // timestamp, fall back to the old "delete everything
                    // except keep_file_id" behavior.
                    _ => true,
                }
        })
        .map(|file| file.id)
        .collect::<Vec<_>>();
    delete_files(client, token, &ids).await?;
    Ok(ids)
}

async fn download_state(
    client: &PutioClient,
    token: &str,
    file_id: u64,
) -> Result<BTreeMap<String, FileStateEntry>> {
    let bytes = download_file(client, token, file_id).await?;
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_slice::<FileStateFile>(&bytes)
        .ok()
        .filter(|file| file.version == 1)
        .map(|file| file.entries)
        .unwrap_or_default())
}

fn latest_profile_files(files: Vec<PutIoFile>) -> BTreeMap<String, PutIoFile> {
    let mut latest: BTreeMap<String, PutIoFile> = BTreeMap::new();
    for file in files {
        if file.file_type == "FOLDER" {
            continue;
        }
        let Some(slug) = parse_profile_slug(&file.name) else {
            continue;
        };
        match latest.get(&slug) {
            Some(existing) if file.updated_at <= existing.updated_at => {}
            _ => {
                latest.insert(slug, file);
            }
        }
    }
    latest
}

pub fn slugify(name: &str) -> Result<String> {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        Err(anyhow!(
            "profile name must include at least one ASCII letter or number"
        ))
    } else {
        Ok(slug)
    }
}

fn profile_filename(slug: &str) -> String {
    format!("{slug}{PROFILE_SUFFIX}")
}

fn parse_profile_slug(filename: &str) -> Option<String> {
    let name = filename.strip_suffix(".json")?;
    let marker = "_file_state";
    let marker_index = name.find(marker)?;
    let suffix = &name[marker_index + marker.len()..];
    if let Some(first) = suffix.chars().next() {
        if first.is_ascii_alphanumeric() || first == '_' {
            return None;
        }
    }
    let slug = &name[..marker_index];
    (!slug.is_empty()).then(|| slug.to_string())
}

fn profile_name_from_slug(cfg: &ConfigStore, slug: &str) -> String {
    let (configured_slug, configured_name) = cfg.file_state_sync_profile();
    if configured_slug == slug && !configured_name.trim().is_empty() {
        configured_name
    } else {
        slug.replace('-', " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_profile_names() {
        assert_eq!(slugify("Test Profile").unwrap(), "test-profile");
        assert_eq!(slugify(" a__b  c ").unwrap(), "a-b-c");
        assert!(slugify("東京").is_err());
    }

    #[test]
    fn parses_putio_renamed_profile_files() {
        assert_eq!(
            parse_profile_slug("xora_file_state.json").as_deref(),
            Some("xora")
        );
        assert_eq!(
            parse_profile_slug("xora_file_state-oNXwC4uT.json").as_deref(),
            Some("xora")
        );
        assert_eq!(
            parse_profile_slug("xora_file_state.oUYkpp7n.json").as_deref(),
            Some("xora")
        );
        assert_eq!(
            parse_profile_slug("xora_file_state (1).json").as_deref(),
            Some("xora")
        );
        assert_eq!(parse_profile_slug("xora_file_state_backup.json"), None);
    }
}
