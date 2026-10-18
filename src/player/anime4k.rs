//! Anime4K selection and application-owned shader assets. No alternate renderer.
use std::{
    cell::Cell,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
#[cfg(test)]
use libmpv2::Mpv;
use serde::{Deserialize, Serialize};

use super::anime4k_assets::FILES;
#[cfg(test)]
use super::mpv_nodes;

const REVISION: &str = "7684e9586f8dcc738af08a1cdceb024cc184f426";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    B = 1,
    C = 2,
    #[serde(rename = "A+A")]
    AA = 3,
    #[serde(rename = "B+B")]
    BB = 4,
    #[serde(rename = "C+A")]
    CA = 5,
    #[default]
    #[serde(other)]
    A = 0,
}

impl Mode {
    pub const ALL: [Self; 6] = [Self::A, Self::B, Self::C, Self::AA, Self::BB, Self::CA];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Hq,
    #[default]
    #[serde(other)]
    Fast,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub mode: Mode,
    pub quality: Quality,
}

impl Settings {
    pub fn chain(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let (main, secondary) = match self.quality {
            Quality::Fast => ("M", "S"),
            Quality::Hq => ("VL", "M"),
        };
        let shader = |name: String| format!("Anime4K_{name}.glsl");
        let restore = |size| shader(format!("Restore_CNN_{size}"));
        let soft = |size| shader(format!("Restore_CNN_Soft_{size}"));
        let upscale = |size| shader(format!("Upscale_CNN_x2_{size}"));
        let mut chain = vec![shader("Clamp_Highlights".into())];
        match self.mode {
            Mode::A | Mode::AA => chain.extend([restore(main), upscale(main)]),
            Mode::B | Mode::BB => chain.extend([soft(main), upscale(main)]),
            Mode::C | Mode::CA => chain.push(shader(format!("Upscale_Denoise_CNN_x2_{main}"))),
        }
        if self.mode == Mode::AA {
            chain.push(restore(secondary));
        }
        chain.extend([
            shader("AutoDownscalePre_x2".into()),
            shader("AutoDownscalePre_x4".into()),
        ]);
        match self.mode {
            Mode::BB => chain.push(soft(secondary)),
            Mode::CA => chain.push(restore(secondary)),
            _ => (),
        }
        chain.push(upscale(secondary));
        chain
    }
}

pub struct Shaders {
    root: PathBuf,
    installed: Cell<bool>,
}

impl Shaders {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            root: config_dir.join("shaders").join("anime4k"),
            installed: Cell::new(false),
        }
    }

    pub fn invalidate(&self) {
        self.installed.set(false);
    }

    fn install(&self) -> Result<PathBuf> {
        let dir = self.root.join(REVISION);
        // Avoid reading the bundle on every file load. Cheap existence checks
        // still repair files removed while the app is running.
        if self.installed.get() && FILES.iter().all(|(name, _)| dir.join(name).is_file()) {
            return Ok(dir);
        }
        self.installed.set(false);
        std::fs::create_dir_all(&dir).context("Cannot create the Anime4K shader directory")?;
        for (name, bytes) in FILES {
            let path = dir.join(name);
            // Compare every file with the embedded bytes, repairing incomplete or
            // corrupted installations as well as handling version upgrades.
            if std::fs::read(&path).is_ok_and(|existing| existing == *bytes) {
                continue;
            }
            let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
            std::fs::write(&tmp, bytes).with_context(|| format!("Cannot install {name}"))?;
            std::fs::rename(&tmp, &path).with_context(|| format!("Cannot replace {name}"))?;
        }
        self.installed.set(true);
        Ok(dir)
    }

    /// Only our versioned directory is owned, never arbitrary Anime4K filenames.
    #[cfg(test)]
    fn compose(&self, current: Vec<String>, selected: Vec<String>) -> Vec<String> {
        current
            .into_iter()
            .filter(|path| !Path::new(path).starts_with(&self.root))
            .chain(selected)
            .collect()
    }

    #[cfg(test)]
    pub fn apply(&self, mpv: &Mpv, settings: &Settings) -> Result<()> {
        let current = mpv_nodes::strings(mpv, "glsl-shaders")?;
        let composed = self.compose(current.clone(), self.selected_paths(settings)?);
        if current != composed {
            mpv_nodes::set_strings(mpv, "glsl-shaders", &composed)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn selected_paths(&self, settings: &Settings) -> Result<Vec<String>> {
        let selected = if settings.enabled {
            let dir = self.install()?;
            settings
                .chain()
                .iter()
                .map(|name| {
                    dir.join(name)
                        .to_str()
                        .map(String::from)
                        .context("Anime4K shader path is not valid UTF-8")
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(selected)
    }
}

pub fn shader_error(prefix: &str, level: &str, text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    matches!(level, "error" | "fatal")
        && (prefix.starts_with("vo/") || prefix.starts_with("libmpv_render"))
        && ["shader", "glsl", "hook"]
            .iter()
            .any(|word| text.contains(word))
}

pub const GLES_WARNING: &str = "Anime4K can't run with your graphics driver right now, so it is paused. Your choice is saved and will work again when the driver allows it.";

pub fn incompatible_context(version: &str, vendor: &str) -> bool {
    version.starts_with("OpenGL ES") && vendor.to_ascii_lowercase().contains("nvidia")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    #[test]
    fn bundled_files_match_the_pinned_manifest() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../assets/anime4k/manifest.json")).unwrap();
        assert_eq!(manifest["revision"], REVISION);
        assert_eq!(manifest["files"].as_array().unwrap().len(), FILES.len());
        for (name, bytes) in FILES {
            let entry = manifest["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["file"] == *name)
                .unwrap();
            assert_eq!(entry["sha256"], format!("{:x}", Sha256::digest(bytes)));
        }
    }

    #[test]
    fn every_chain_is_bundled_and_secondary_restoration_is_ordered_correctly() {
        for quality in [Quality::Fast, Quality::Hq] {
            for mode in Mode::ALL {
                let chain = Settings {
                    enabled: true,
                    mode,
                    quality,
                }
                .chain();
                assert!(chain
                    .iter()
                    .all(|name| FILES.iter().any(|(f, _)| f == name)));
                let pre = chain
                    .iter()
                    .position(|name| name.contains("Pre_x2"))
                    .unwrap();
                match mode {
                    Mode::AA => assert!(chain[pre - 1].contains("Restore")),
                    Mode::BB | Mode::CA => assert!(chain[pre + 2].contains("Restore")),
                    _ => (),
                }
            }
        }
        assert!(Settings::default().chain().is_empty());
    }

    #[test]
    fn installation_repairs_each_file_and_off_preserves_custom_shaders() {
        let base = std::env::temp_dir().join(format!("putmpv-anime4k-test-{}", std::process::id()));
        let shaders = Shaders::new(&base);
        let dir = shaders.install().unwrap();
        std::fs::write(dir.join(FILES[1].0), b"corrupt").unwrap();
        shaders.install().unwrap();
        assert_eq!(std::fs::read(dir.join(FILES[1].0)).unwrap(), b"corrupt");
        shaders.invalidate();
        shaders.install().unwrap();
        assert_eq!(std::fs::read(dir.join(FILES[1].0)).unwrap(), FILES[1].1);
        std::fs::write(dir.join(FILES[1].0), b"corrupt").unwrap();
        std::fs::remove_file(dir.join(FILES[2].0)).unwrap();
        shaders.install().unwrap();
        for (name, bytes) in FILES {
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), *bytes);
        }
        let custom = vec![
            "/custom/Anime4K_Clamp_Highlights.glsl".into(),
            "/custom/color:grade.glsl".into(),
        ];
        let mut current = custom.clone();
        current.insert(1, dir.join(FILES[0].0).to_str().unwrap().into());
        current.push(
            shaders
                .root
                .join("old-version/old.glsl")
                .to_str()
                .unwrap()
                .into(),
        );
        assert_eq!(shaders.compose(current, vec![]), custom);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn diagnostics_ignore_non_shader_errors() {
        assert!(shader_error(
            "vo/libmpv",
            "error",
            "Shader compilation failed"
        ));
        assert!(!shader_error(
            "ffmpeg",
            "error",
            "Could not load shader.mkv"
        ));
        assert!(!shader_error("vo/libmpv", "info", "Compiling shader"));
    }

    #[test]
    fn only_nvidia_gles_is_blocked() {
        assert!(incompatible_context(
            "OpenGL ES 3.2 NVIDIA",
            "NVIDIA Corporation"
        ));
        assert!(!incompatible_context("4.6 NVIDIA", "NVIDIA Corporation"));
        assert!(!incompatible_context("OpenGL ES 3.2 Mesa", "Mesa"));
    }
}
