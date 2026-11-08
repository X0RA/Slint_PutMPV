//! Pass configuration through mpv's own parser, including profiles and quoting.
use std::{io::Write, path::PathBuf};

use anyhow::{bail, Context, Result};
use libmpv2::Mpv;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigMode {
    #[default]
    Disabled,
    File,
    Text,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MpvConfig {
    pub mode: ConfigMode,
    pub path: String,
    pub text: String,
}

impl Default for MpvConfig {
    fn default() -> Self {
        Self {
            mode: ConfigMode::Disabled,
            path: default_path().to_string_lossy().into_owned(),
            text: String::new(),
        }
    }
}

fn default_path() -> PathBuf {
    if let Some(home) = std::env::var_os("MPV_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home).join("mpv.conf");
    }
    #[cfg(windows)]
    {
        if let Ok(exe) = std::env::current_exe() {
            let portable = exe.parent().unwrap().join("portable_config");
            if portable.is_dir() {
                return portable.join("mpv.conf");
            }
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("mpv/mpv.conf");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("mpv/mpv.conf");
    }
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~"));
    let legacy = home.join(".mpv/mpv.conf");
    let modern = home.join(".config/mpv/mpv.conf");
    if !modern.exists() && legacy.exists() {
        legacy
    } else {
        modern
    }
}

fn expand_path(path: &str) -> Result<PathBuf> {
    let path = path.trim();
    if path.is_empty() {
        bail!("Enter the path to an mpv.conf file.");
    }
    let path = if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        directories::BaseDirs::new()
            .context("Cannot determine your home directory")?
            .home_dir()
            .join(rest)
    } else {
        PathBuf::from(path)
    };
    path.canonicalize()
        .with_context(|| format!("Cannot open {}", path.display()))
}

pub fn create_mpv(config: &MpvConfig) -> Result<Mpv> {
    // Keep the temporary file alive through initialization, then remove it. It
    // is private to this invocation, so validation cannot race with playback.
    let mut temporary = None;
    let path = match config.mode {
        ConfigMode::Disabled => None,
        ConfigMode::File => Some(expand_path(&config.path)?),
        ConfigMode::Text => {
            let mut file = tempfile::Builder::new()
                .prefix("putmpv-")
                .suffix(".conf")
                .tempfile()?;
            file.write_all(config.text.as_bytes())?;
            file.flush()?;
            let path = file.path().to_path_buf();
            temporary = Some(file);
            Some(path)
        }
    };
    let filename = path
        .as_ref()
        .map(|p| p.to_str().context("Config path is not valid UTF-8"))
        .transpose()?;
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("config", "no")?;
        if let Some(filename) = filename {
            init.load_config(filename)?;
        }
        // Do not let config=yes in the selected source load another config.
        init.set_option("config", "no")?;
        // The app owns the window and rendering context.
        init.set_option("vo", "libmpv")?;
        init.set_option("terminal", "no")?;
        Ok(())
    })
    .context("Could not initialize mpv with this configuration")?;
    drop(temporary);
    Ok(mpv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_parser_loads_text_and_file_and_keeps_embedded_output() {
        let text =
            "# pasted config\nspeed=1.5\nsub-font=\"Example Font\"\nvo=null\n[slower]\nspeed=0.5\n";
        let config = MpvConfig {
            mode: ConfigMode::Text,
            text: text.into(),
            ..Default::default()
        };
        let mpv = create_mpv(&config).unwrap();
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.5);
        assert_eq!(
            mpv.get_property::<String>("sub-font").unwrap(),
            "Example Font"
        );
        assert_eq!(mpv.get_property::<String>("vo").unwrap(), "libmpv");
        mpv.command("apply-profile", &["slower"]).unwrap();
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 0.5);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"speed=1.75\n").unwrap();
        let config = MpvConfig {
            mode: ConfigMode::File,
            path: file.path().to_str().unwrap().into(),
            ..config
        };
        let mpv = create_mpv(&config).unwrap();
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.75);
        let mpv = create_mpv(&MpvConfig {
            mode: ConfigMode::Disabled,
            ..config
        })
        .unwrap();
        assert_eq!(mpv.get_property::<f64>("speed").unwrap(), 1.0);
    }

    #[test]
    fn old_settings_default_to_disabled_and_missing_files_report_errors() {
        let config: crate::storage::config::ConfigFile = serde_json::from_str("{}").unwrap();
        assert_eq!(config.mpv_config.mode, ConfigMode::Disabled);
        let config = MpvConfig {
            mode: ConfigMode::File,
            path: String::new(),
            ..Default::default()
        };
        assert!(create_mpv(&config).is_err());
        let encoded = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<MpvConfig>(&encoded).unwrap(), config);
    }
}
