//! Spatial enhancements for the existing libmpv OpenGL renderer.
use super::{anime4k, mpv_nodes};
use anyhow::{Context, Result};
use libmpv2::Mpv;
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Upscaler {
    Lightweight = 1,
    FsrcnnxBalanced = 2,
    FsrcnnxHigh = 3,
    EwaLanczosSharp = 4,
    #[default]
    #[serde(other)]
    Standard = 0,
}
impl Upscaler {
    pub const ALL: [Self; 5] = [
        Self::Standard,
        Self::Lightweight,
        Self::FsrcnnxBalanced,
        Self::FsrcnnxHigh,
        Self::EwaLanczosSharp,
    ];
    pub fn neural(self) -> bool {
        matches!(self, Self::FsrcnnxBalanced | Self::FsrcnnxHigh)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub upscaler: Upscaler,
    pub refinement: bool,
    pub chroma: bool,
    pub downscale: bool,
    pub grain: u8,
}
impl Settings {
    /// Keep built-in scaling choices when the context cannot run bundled shaders.
    pub fn without_shaders(&self) -> Self {
        Self {
            grain: self.grain,
            upscaler: if self.upscaler.neural() {
                Upscaler::Standard
            } else {
                self.upscaler
            },
            ..Default::default()
        }
    }
    pub fn has_shaders(&self) -> bool {
        self.upscaler.neural() || self.refinement || self.chroma || self.downscale
    }
    pub fn chain(&self) -> Vec<&'static str> {
        let mut files = Vec::new();
        match self.upscaler {
            Upscaler::FsrcnnxBalanced => files.push("FSRCNNX_x2_8-0-4-1.glsl"),
            Upscaler::FsrcnnxHigh => files.push("FSRCNNX_x2_16-0-4-1.glsl"),
            _ => (),
        }
        if self.chroma {
            files.push("CfL_Prediction_Lite.glsl");
        }
        if self.refinement {
            files.push("SSimSuperRes.glsl");
        }
        if self.downscale {
            files.push("SSimDownscaler.glsl");
        }
        files
    }
}

const FILES: &[(&str, &[u8])] = &[
    (
        "FSRCNNX_x2_8-0-4-1.glsl",
        include_bytes!("../../assets/enhancements/FSRCNNX_x2_8-0-4-1.glsl"),
    ),
    (
        "FSRCNNX_x2_16-0-4-1.glsl",
        include_bytes!("../../assets/enhancements/FSRCNNX_x2_16-0-4-1.glsl"),
    ),
    (
        "CfL_Prediction_Lite.glsl",
        include_bytes!("../../assets/enhancements/CfL_Prediction_Lite.glsl"),
    ),
    (
        "SSimSuperRes.glsl",
        include_bytes!("../../assets/enhancements/SSimSuperRes.glsl"),
    ),
    (
        "SSimDownscaler.glsl",
        include_bytes!("../../assets/enhancements/SSimDownscaler.glsl"),
    ),
    (
        "CfL-LICENSE",
        include_bytes!("../../assets/enhancements/CfL-LICENSE"),
    ),
    (
        "LGPL-3.0.txt",
        include_bytes!("../../assets/enhancements/LGPL-3.0.txt"),
    ),
    (
        "GPL-3.0.txt",
        include_bytes!("../../assets/enhancements/GPL-3.0.txt"),
    ),
];
pub struct Shaders {
    root: PathBuf,
    installed: Cell<bool>,
    scale_original: RefCell<Option<String>>,
    downscale_original: RefCell<Option<(String, bool)>>,
    grain_original: RefCell<Option<(bool, i64, f64)>>,
}
impl Shaders {
    pub fn new(config: &Path) -> Self {
        Self {
            root: config.join("shaders/enhancements"),
            installed: Cell::new(false),
            scale_original: RefCell::new(None),
            downscale_original: RefCell::new(None),
            grain_original: RefCell::new(None),
        }
    }
    pub fn invalidate(&self) {
        self.installed.set(false);
    }
    fn install(&self) -> Result<PathBuf> {
        use sha2::{Digest, Sha256};
        // Content-addressed bundle directory changes whenever an upstream pin changes.
        let revision = format!(
            "{:x}",
            Sha256::digest(include_bytes!("../../assets/enhancements/manifest.json"))
        );
        let dir = self.root.join(&revision[..16]);
        if self.installed.get() && FILES.iter().all(|(name, _)| dir.join(name).is_file()) {
            return Ok(dir);
        }
        self.installed.set(false);
        std::fs::create_dir_all(&dir).context("Cannot create video shader directory")?;
        for (name, bytes) in FILES {
            let path = dir.join(name);
            if std::fs::read(&path).is_ok_and(|v| v == *bytes) {
                continue;
            }
            let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
            std::fs::write(&temp, bytes)?;
            std::fs::rename(temp, path).with_context(|| format!("Cannot install {name}"))?;
        }
        self.installed.set(true);
        Ok(dir)
    }
    fn apply_grain(&self, mpv: &Mpv, amount: u8) -> Result<()> {
        if amount > 0 {
            if self.grain_original.borrow().is_none() {
                *self.grain_original.borrow_mut() = Some((
                    mpv.get_property("deband")?,
                    mpv.get_property("deband-iterations")?,
                    mpv.get_property("deband-grain")?,
                ));
            }
            let (enabled, iterations, _) = self.grain_original.borrow().unwrap();
            // Zero smoothing iterations when we enabled the deband pass solely
            // for noise. Preserve any debanding the user already had enabled.
            mpv.set_property("deband-iterations", if enabled { iterations } else { 0 })?;
            mpv.set_property("deband-grain", f64::from(amount.min(100)) * 2.56)?;
            mpv.set_property("deband", true)?;
        } else {
            let previous = *self.grain_original.borrow();
            if let Some((enabled, iterations, grain)) = previous {
                mpv.set_property("deband", enabled)?;
                mpv.set_property("deband-iterations", iterations)?;
                mpv.set_property("deband-grain", grain)?;
                self.grain_original.borrow_mut().take();
            }
        }
        Ok(())
    }

    pub fn apply(
        &self,
        mpv: &Mpv,
        anime: &anime4k::Shaders,
        anime_settings: &anime4k::Settings,
        settings: &Settings,
    ) -> Result<()> {
        let selected = if settings.has_shaders() {
            let dir = self.install()?;
            settings
                .chain()
                .iter()
                .map(|name| {
                    dir.join(name)
                        .to_str()
                        .map(String::from)
                        .context("Invalid video shader path")
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        if settings.downscale {
            if self.downscale_original.borrow().is_none() {
                *self.downscale_original.borrow_mut() = Some((
                    mpv.get_property("dscale")?,
                    mpv.get_property("linear-downscaling")?,
                ));
            }
            mpv.set_property("dscale", "mitchell")?;
            mpv.set_property("linear-downscaling", false)?;
        } else {
            let previous = self.downscale_original.borrow().clone();
            if let Some((scale, linear)) = previous {
                mpv.set_property("dscale", scale.as_str())?;
                mpv.set_property("linear-downscaling", linear)?;
                self.downscale_original.borrow_mut().take();
            }
        }
        let current = mpv_nodes::strings(mpv, "glsl-shaders")
            .context("Cannot read the current video shaders")?;
        let composed: Vec<_> = current
            .iter()
            .filter(|path| {
                !Path::new(path).starts_with(&self.root)
                    && !Path::new(path).starts_with(anime.root())
            })
            .cloned()
            .chain(anime.selected_paths(anime_settings)?)
            .chain(selected)
            .collect();
        if current != composed {
            mpv_nodes::set_strings(mpv, "glsl-shaders", &composed)
                .context("Cannot apply video enhancement shaders")?;
        }
        let scale = match settings.upscaler {
            Upscaler::Standard => None,
            Upscaler::Lightweight => Some("spline36"),
            _ => Some("ewa_lanczossharp"),
        };
        if let Some(scale) = scale {
            if self.scale_original.borrow().is_none() {
                *self.scale_original.borrow_mut() = Some(mpv.get_property("scale")?);
            }
            mpv.set_property("scale", scale)
                .context("Cannot select video scaler")?;
        } else {
            let previous = self.scale_original.borrow().clone();
            if let Some(previous) = previous {
                mpv.set_property("scale", previous.as_str())
                    .context("Cannot restore video scaler")?;
                self.scale_original.borrow_mut().take();
            }
        }
        self.apply_grain(mpv, settings.grain)
            .context("Cannot adjust film grain")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grain_preserves_debanding_and_restores_original_options() {
        let engine = crate::player::PlayerEngine::new().unwrap();
        let mpv = engine.mpv();
        let root = std::env::temp_dir().join("putmpv-grain-settings-test");
        let shaders = Shaders::new(&root);
        let anime = anime4k::Shaders::new(&root);
        let anime_settings = anime4k::Settings::default();
        for originally_enabled in [false, true] {
            mpv.set_property("deband", originally_enabled).unwrap();
            mpv.set_property("deband-iterations", 3i64).unwrap();
            mpv.set_property("deband-grain", 12.5).unwrap();
            shaders
                .apply(mpv, &anime, &anime_settings, &Settings::default())
                .unwrap();
            assert_eq!(
                mpv.get_property::<bool>("deband").unwrap(),
                originally_enabled
            );
            for grain in [25, 100, 25] {
                let settings = Settings {
                    grain,
                    ..Default::default()
                };
                for _ in 0..2 {
                    shaders
                        .apply(mpv, &anime, &anime_settings, &settings)
                        .unwrap();
                }
                assert!(mpv.get_property::<bool>("deband").unwrap());
                assert_eq!(
                    mpv.get_property::<i64>("deband-iterations").unwrap(),
                    if originally_enabled { 3 } else { 0 }
                );
                assert!(
                    (mpv.get_property::<f64>("deband-grain").unwrap() - f64::from(grain) * 2.56)
                        .abs()
                        < 0.001
                );
            }
            shaders
                .apply(mpv, &anime, &anime_settings, &Settings::default())
                .unwrap();
            assert_eq!(
                mpv.get_property::<bool>("deband").unwrap(),
                originally_enabled
            );
            assert_eq!(mpv.get_property::<i64>("deband-iterations").unwrap(), 3);
            assert_eq!(mpv.get_property::<f64>("deband-grain").unwrap(), 12.5);
        }
    }

    #[test]
    fn blocked_context_retains_builtin_scalers_but_no_shader_flags() {
        for upscaler in Upscaler::ALL {
            let requested = Settings {
                upscaler,
                refinement: true,
                chroma: true,
                downscale: true,
                grain: 35,
            };
            let effective = requested.without_shaders();
            assert!(!effective.has_shaders());
            assert_eq!(
                effective.upscaler,
                if upscaler.neural() {
                    Upscaler::Standard
                } else {
                    upscaler
                }
            );
            assert!(requested.refinement);
            assert_eq!(effective.grain, 35);
        }
    }

    #[test]
    fn standard_preserves_original_scaler_and_restores_after_opt_in() {
        let engine = crate::player::PlayerEngine::new().unwrap();
        let mpv = engine.mpv();
        let root = std::env::temp_dir().join("putmpv-standard-scaler-test");
        let anime = anime4k::Shaders::new(&root);
        let shaders = Shaders::new(&root);
        let default = Settings::default();
        let anime_settings = anime4k::Settings::default();
        for original in ["lanczos", "bilinear"] {
            mpv.set_property("scale", original).unwrap();
            for _ in 0..2 {
                shaders
                    .apply(mpv, &anime, &anime_settings, &default)
                    .unwrap();
                assert_eq!(mpv.get_property::<String>("scale").unwrap(), original);
            }
            for (upscaler, expected) in [
                (Upscaler::EwaLanczosSharp, "ewa_lanczossharp"),
                (Upscaler::Lightweight, "spline36"),
            ] {
                shaders
                    .apply(
                        mpv,
                        &anime,
                        &anime_settings,
                        &Settings {
                            upscaler,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                assert_eq!(mpv.get_property::<String>("scale").unwrap(), expected);
            }
            shaders
                .apply(mpv, &anime, &anime_settings, &default)
                .unwrap();
            assert_eq!(mpv.get_property::<String>("scale").unwrap(), original);
        }
    }

    #[test]
    fn chain_switching_preserves_custom_paths_and_restores_downscale_options() {
        let engine = crate::player::PlayerEngine::new().unwrap();
        let mpv = engine.mpv();
        let root =
            std::env::temp_dir().join(format!("putmpv-enhancement-chain-{}", std::process::id()));
        let anime = anime4k::Shaders::new(&root);
        let shaders = Shaders::new(&root);
        let custom = vec!["/custom/colour; grade:one.glsl".to_owned()];
        super::super::mpv_nodes::set_strings(mpv, "glsl-shaders", &custom).unwrap();
        mpv.set_property("dscale", "lanczos").unwrap();
        mpv.set_property("linear-downscaling", true).unwrap();
        let mut settings = Settings {
            upscaler: Upscaler::FsrcnnxHigh,
            refinement: true,
            chroma: true,
            downscale: true,
            grain: 0,
        };
        shaders
            .apply(mpv, &anime, &anime4k::Settings::default(), &settings)
            .unwrap();
        let chain = super::super::mpv_nodes::strings(mpv, "glsl-shaders").unwrap();
        assert_eq!(chain.len(), 5);
        assert_eq!(chain[0], custom[0]);
        assert!(chain[1].ends_with("FSRCNNX_x2_16-0-4-1.glsl"));
        assert!(!mpv.get_property::<bool>("linear-downscaling").unwrap());
        settings.upscaler = Upscaler::Standard;
        shaders
            .apply(
                mpv,
                &anime,
                &anime4k::Settings {
                    enabled: true,
                    ..Default::default()
                },
                &settings,
            )
            .unwrap();
        let chain = super::super::mpv_nodes::strings(mpv, "glsl-shaders").unwrap();
        assert_eq!(chain[0], custom[0]);
        assert!(!chain.iter().any(|p| p.contains("FSRCNNX")));
        assert!(chain.iter().any(|p| p.contains("Anime4K")));
        shaders
            .apply(
                mpv,
                &anime,
                &anime4k::Settings::default(),
                &Settings::default(),
            )
            .unwrap();
        assert_eq!(
            super::super::mpv_nodes::strings(mpv, "glsl-shaders").unwrap(),
            custom
        );
        assert_eq!(mpv.get_property::<String>("dscale").unwrap(), "lanczos");
        assert!(mpv.get_property::<bool>("linear-downscaling").unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_assets_and_repair() {
        use sha2::{Digest, Sha256};
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/enhancements/manifest.json")).unwrap();
        for entry in manifest["files"].as_array().unwrap() {
            let (_, bytes) = FILES
                .iter()
                .find(|(name, _)| *name == entry["file"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                entry["sha256"].as_str().unwrap()
            );
        }
        let cfl = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["file"] == "CfL_Prediction_Lite.glsl")
            .unwrap();
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(include_bytes!(
                    "../../assets/enhancements/upstream/CfL_Prediction_Lite.glsl"
                ))
            ),
            cfl["upstream_sha256"].as_str().unwrap()
        );
        let root = std::env::temp_dir().join(format!("putmpv-enhancements-{}", std::process::id()));
        let shaders = Shaders::new(&root);
        let dir = shaders.install().unwrap();
        std::fs::write(dir.join(FILES[0].0), b"bad").unwrap();
        shaders.invalidate();
        shaders.install().unwrap();
        assert_eq!(std::fs::read(dir.join(FILES[0].0)).unwrap(), FILES[0].1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
