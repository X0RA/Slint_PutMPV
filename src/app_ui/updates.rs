use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use anyhow::anyhow;
use anyhow::{bail, Context, Result};
use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::Deserialize;
#[cfg(windows)]
use sha2::{Digest, Sha256};
use slint::ComponentHandle;
use tokio::runtime::Runtime;

use crate::AppWindow;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/X0RA/Slint_PutMPV/releases/latest";
const LINUX_INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/X0RA/Slint_PutMPV/main/scripts/install-linux.sh | bash";
const MACOS_INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/X0RA/Slint_PutMPV/main/scripts/install-macos.sh | bash";

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug)]
struct LatestUpdate {
    version: String,
    installer_url: Option<String>,
    installer_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub(crate) fn install(app: &AppWindow, rt: &Arc<Runtime>) {
    let latest = Arc::new(Mutex::new(None::<LatestUpdate>));

    app.set_update_install_supported(cfg!(windows));

    app.on_updates_check({
        let weak = app.as_weak();
        let rt = rt.clone();
        let latest = latest.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let current_version = app.get_app_version().to_string();
            app.set_update_busy(true);
            app.set_update_available(false);
            app.set_latest_version("".into());
            app.set_update_action_enabled(false);
            app.set_update_action_label(default_action_label().into());
            app.set_update_status("Checking GitHub releases...".into());

            let weak = weak.clone();
            let latest = latest.clone();
            rt.spawn(async move {
                let result = check_latest(&current_version).await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_update_busy(false);
                    match result {
                        Ok(state) => apply_check_result(&app, state, &latest),
                        Err(err) => {
                            app.set_update_status(format!("Update check failed: {err}").into());
                            app.set_update_available(false);
                            app.set_update_action_enabled(false);
                        }
                    }
                });
            });
        }
    });

    app.on_updates_install({
        let weak = app.as_weak();
        let rt = rt.clone();
        let latest = latest.clone();
        move || {
            let Some(update) = latest.lock().ok().and_then(|guard| guard.clone()) else {
                if let Some(app) = weak.upgrade() {
                    app.set_update_status("Check for updates before installing.".into());
                }
                return;
            };

            let Some(app) = weak.upgrade() else {
                return;
            };
            app.set_update_busy(true);
            app.set_update_action_enabled(false);
            app.set_update_status(format!("Downloading PutMPV {} installer...", update.version).into());

            let weak = weak.clone();
            rt.spawn(async move {
                let result = download_and_launch(update).await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_update_busy(false);
                    match result {
                        #[cfg(windows)]
                        Ok(InstallAction::QuitApp { checksum_verified }) => {
                            let message = if checksum_verified {
                                "Installer launched. PutMPV will close now."
                            } else {
                                "Installer launched (checksum unavailable). PutMPV will close now."
                            };
                            app.set_update_status(message.into());
                            slint::quit_event_loop().ok();
                        }
                        Ok(InstallAction::Manual(message)) => {
                            app.set_update_status(message.into());
                            app.set_update_action_enabled(false);
                        }
                        Err(err) => {
                            app.set_update_status(format!("Install failed: {err}").into());
                            app.set_update_action_enabled(cfg!(windows));
                        }
                    }
                });
            });
        }
    });
}

#[derive(Debug)]
struct CheckState {
    latest_version: String,
    update_available: bool,
    status: String,
    action_enabled: bool,
    action_label: String,
    update: Option<LatestUpdate>,
}

async fn check_latest(current_version: &str) -> Result<CheckState> {
    let current = parse_version(current_version)
        .with_context(|| format!("current version '{current_version}' is not valid semver"))?;
    let release = fetch_latest_release(current_version).await?;
    let latest = parse_version(&release.tag_name)
        .with_context(|| format!("latest release tag '{}' is not valid semver", release.tag_name))?;
    let latest_label = latest.to_string();

    if latest <= current {
        return Ok(CheckState {
            latest_version: latest_label.clone(),
            update_available: false,
            status: format!("You are up to date. Latest release is {latest_label}."),
            action_enabled: false,
            action_label: default_action_label().to_string(),
            update: None,
        });
    }

    let installer = select_windows_installer(&release.assets, &latest_label);
    let platform_message = platform_update_message(&latest_label, &release.html_url, installer.as_ref());
    let action_enabled = cfg!(windows) && installer.is_some();

    Ok(CheckState {
        latest_version: latest_label.clone(),
        update_available: true,
        status: platform_message,
        action_enabled,
        action_label: if cfg!(windows) {
            "Install update".to_string()
        } else {
            default_action_label().to_string()
        },
        update: installer.map(|asset| LatestUpdate {
            version: latest_label,
            installer_url: Some(asset.browser_download_url.clone()),
            installer_digest: asset.digest.clone(),
        }),
    })
}

async fn fetch_latest_release(current_version: &str) -> Result<GitHubRelease> {
    let client = reqwest::Client::builder()
        .user_agent(format!("PutMPV/{current_version}"))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, format!("PutMPV/{current_version}"))
        .send()
        .await
        .context("failed to contact GitHub")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub returned {status}: {}", body.trim());
    }
    response.json().await.context("failed to parse GitHub release")
}

fn apply_check_result(
    app: &AppWindow,
    state: CheckState,
    latest: &Arc<Mutex<Option<LatestUpdate>>>,
) {
    app.set_latest_version(state.latest_version.into());
    app.set_update_available(state.update_available);
    app.set_update_status(state.status.into());
    app.set_update_action_label(state.action_label.into());
    app.set_update_action_enabled(state.action_enabled);
    if let Ok(mut guard) = latest.lock() {
        *guard = state.update;
    }
}

fn parse_version(input: &str) -> Result<Version, semver::Error> {
    Version::parse(input.trim().trim_start_matches('v'))
}

fn select_windows_installer<'a>(assets: &'a [GitHubAsset], version: &str) -> Option<&'a GitHubAsset> {
    let exact = format!("PutMPV-{version}-Setup.exe");
    assets.iter().find(|asset| asset.name == exact)
}

fn platform_update_message(
    latest_version: &str,
    release_url: &str,
    installer: Option<&&GitHubAsset>,
) -> String {
    if cfg!(windows) {
        if installer.is_some() {
            format!("Version {latest_version} is available. Use Install update to download and launch the Windows installer.")
        } else {
            format!("Version {latest_version} is available, but no Windows installer asset was found. Download it from {release_url}.")
        }
    } else if cfg!(target_os = "macos") {
        format!("Version {latest_version} is available. Update with:\n{MACOS_INSTALL}")
    } else if cfg!(target_os = "linux") {
        format!("Version {latest_version} is available. Update with:\nyay -S putmpv-bin\nor\n{LINUX_INSTALL}")
    } else {
        format!("Version {latest_version} is available. Download it from {release_url}.")
    }
}

fn default_action_label() -> &'static str {
    if cfg!(windows) {
        "Install update"
    } else {
        "Download"
    }
}

enum InstallAction {
    #[cfg(windows)]
    QuitApp { checksum_verified: bool },
    Manual(String),
}

#[cfg(windows)]
async fn download_and_launch(update: LatestUpdate) -> Result<InstallAction> {
    let installer_url = update
        .installer_url
        .ok_or_else(|| anyhow!("release did not include a Windows installer"))?;
    let installer_path = download_installer(&update.version, &installer_url).await?;
    let checksum_verified = match update.installer_digest.as_deref() {
        Some(digest) => {
            verify_sha256(&installer_path, digest).await?;
            true
        }
        None => false,
    };
    launch_installer(&installer_path)?;
    Ok(InstallAction::QuitApp { checksum_verified })
}

#[cfg(not(windows))]
async fn download_and_launch(_update: LatestUpdate) -> Result<InstallAction> {
    let message = if cfg!(target_os = "macos") {
        format!("Update with:\n{MACOS_INSTALL}")
    } else if cfg!(target_os = "linux") {
        format!("Update with:\nyay -S putmpv-bin\nor\n{LINUX_INSTALL}")
    } else {
        "Use the latest GitHub release to update PutMPV.".to_string()
    };
    Ok(InstallAction::Manual(message))
}

#[cfg(windows)]
async fn download_installer(version: &str, installer_url: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("PutMPV-updates");
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(format!("PutMPV-{version}-Setup.exe"));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .get(installer_url)
        .header(USER_AGENT, format!("PutMPV/{version}"))
        .send()
        .await
        .context("failed to download installer")?;
    let status = response.status();
    if !status.is_success() {
        bail!("installer download returned {status}");
    }
    let bytes = response.bytes().await.context("failed to read installer")?;
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

#[cfg(windows)]
async fn verify_sha256(path: &PathBuf, digest: &str) -> Result<()> {
    let expected = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("unsupported asset digest format"))?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        bail!("installer checksum did not match GitHub asset digest")
    }
}

#[cfg(windows)]
fn launch_installer(path: &PathBuf) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let operation = wide(OsStr::new("runas"));
    let file = wide(path.as_os_str());
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        bail!("Windows refused to launch installer, ShellExecute error {result:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GitHubAsset {
        GitHubAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        }
    }

    #[test]
    fn parses_version_tags() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::parse("1.2.3").unwrap());
        assert_eq!(
            parse_version("1.0.0-dev+abcdef1").unwrap(),
            Version::parse("1.0.0-dev+abcdef1").unwrap()
        );
    }

    #[test]
    fn release_is_newer_than_matching_dev_build() {
        let release = parse_version("1.0.0").unwrap();
        let dev = parse_version("1.0.0-dev+abcdef1").unwrap();
        assert!(release > dev);
    }

    #[test]
    fn compares_patch_versions() {
        assert!(parse_version("1.0.1").unwrap() > parse_version("1.0.0").unwrap());
        assert_eq!(parse_version("1.0.0").unwrap(), parse_version("1.0.0").unwrap());
    }

    #[test]
    fn selects_exact_installer_first() {
        let assets = vec![
            asset("PutMPV-1.0.0-Setup.exe"),
            asset("PutMPV-1.2.3-Setup.exe"),
            asset("putmpv-windows-x86_64.exe"),
        ];
        assert_eq!(
            select_windows_installer(&assets, "1.2.3").unwrap().name,
            "PutMPV-1.2.3-Setup.exe"
        );
    }

    #[test]
    fn ignores_mismatched_version_installer() {
        let assets = vec![asset("libmpv-2.dll"), asset("PutMPV-1.2.2-Setup.exe")];
        assert!(select_windows_installer(&assets, "1.2.3").is_none());
    }

    #[test]
    fn ignores_non_installer_assets() {
        let assets = vec![asset("putmpv-windows-x86_64.exe"), asset("libmpv-2.dll")];
        assert!(select_windows_installer(&assets, "1.2.3").is_none());
    }
}
