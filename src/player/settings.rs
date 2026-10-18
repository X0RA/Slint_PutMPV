//! Player preferences are session state. Only explicitly locked sections reach disk.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub gamma: f64,
    pub hue: f64,
    pub speed: f64,
    pub preserve_pitch: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            brightness: 0.,
            contrast: 0.,
            saturation: 0.,
            gamma: 0.,
            hue: 0.,
            speed: 1.,
            preserve_pitch: true,
        }
    }
}

pub const FREQUENCIES: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
// Keep the original preset IDs stable. ID 4 remains Custom in saved settings.
pub const PRESETS: [[f64; 10]; 6] = [
    [0.; 10],
    [-2., -1., 0., 1., 2., 3., 3., 2., 0., -1.],
    [6., 5., 4., 2., 0., 0., 0., 0., 0., 0.],
    [0., 0., 0., 0., 0., 1., 2., 4., 5., 6.],
    [0.; 10],
    [-4., -3., -1., 0., 1., 2., 2., 1., -1., -3.],
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AudioSettings {
    pub delay: f64,
    pub eq_enabled: bool,
    pub bands: [f64; 10],
    /// 0 Flat, 1 Vocal, 2 Bass boost, 3 Treble, 4 Custom, 5 Night.
    pub preset: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub scale: f64,
    pub font: String,
    pub color: String,
    pub background_enabled: bool,
    pub background_color: String,
    pub background_opacity: f64,
    pub position: f64,
    pub delay: f64,
    pub override_style: bool,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self {
            scale: 1.,
            font: "sans-serif".into(),
            color: "#FFFFFF".into(),
            background_enabled: false,
            background_color: "#000000".into(),
            background_opacity: 75.,
            position: 100.,
            delay: 0.,
            override_style: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<SubtitleSettings>,
}

impl SavedSettings {
    pub fn is_empty(&self) -> bool {
        self.video.is_none() && self.audio.is_none() && self.subtitles.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub video: VideoSettings,
    pub audio: AudioSettings,
    pub subtitles: SubtitleSettings,
}

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub values: Settings,
    pub locked: [bool; 3],
}

impl Session {
    pub fn from_saved(saved: SavedSettings) -> Self {
        let locked = [
            saved.video.is_some(),
            saved.audio.is_some(),
            saved.subtitles.is_some(),
        ];
        let mut values = Settings {
            video: saved.video.unwrap_or_default(),
            audio: saved.audio.unwrap_or_default(),
            subtitles: saved.subtitles.unwrap_or_default(),
        };
        values.sanitize();
        Self { values, locked }
    }

    pub fn saved(&self) -> SavedSettings {
        SavedSettings {
            video: self.locked[0].then(|| self.values.video.clone()),
            audio: self.locked[1].then(|| self.values.audio.clone()),
            subtitles: self.locked[2].then(|| self.values.subtitles.clone()),
        }
    }

    pub fn reset(&mut self, section: usize) {
        match section {
            0 => self.values.video = VideoSettings::default(),
            1 => self.values.audio = AudioSettings::default(),
            2 => self.values.subtitles = SubtitleSettings::default(),
            _ => (),
        }
    }
}

fn bounded(value: f64, min: f64, max: f64, default: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

pub fn rgb(text: &str) -> Result<(u8, u8, u8)> {
    let value = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if value.len() != 6 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Use a six-digit color such as #FFFFFF.");
    }
    let n = u32::from_str_radix(value, 16)?;
    Ok(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

pub fn mpv_color(text: &str, opacity: f64) -> Result<String> {
    let (r, g, b) = rgb(text)?;
    let a = (bounded(opacity, 0., 100., 100.) / 100. * 255.).round() as u8;
    Ok(format!("#{a:02X}{r:02X}{g:02X}{b:02X}"))
}

impl Settings {
    pub fn number(&mut self, key: &str, value: f64) -> Result<()> {
        if !value.is_finite() {
            bail!("Enter a finite number.");
        }
        let (slot, min, max) = match key {
            "brightness" => (&mut self.video.brightness, -100., 100.),
            "contrast" => (&mut self.video.contrast, -100., 100.),
            "saturation" => (&mut self.video.saturation, -100., 100.),
            "gamma" => (&mut self.video.gamma, -100., 100.),
            "hue" => (&mut self.video.hue, -100., 100.),
            "speed" => (&mut self.video.speed, 0.25, 4.),
            "audio-delay" => (&mut self.audio.delay, -10., 10.),
            "sub-scale" => (&mut self.subtitles.scale, 0.5, 3.),
            "sub-position" => (&mut self.subtitles.position, 0., 100.),
            "sub-delay" => (&mut self.subtitles.delay, -3600., 3600.),
            "sub-opacity" => (&mut self.subtitles.background_opacity, 0., 100.),
            _ => {
                if let Some(index) = key
                    .strip_prefix("eq-")
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|i| *i < 10)
                {
                    self.audio.bands[index] = value.clamp(-12., 12.);
                    self.audio.preset = 4;
                    return Ok(());
                }
                bail!("Unknown player setting.");
            }
        };
        *slot = value.clamp(min, max);
        Ok(())
    }

    pub fn text(&mut self, key: &str, text: &str) -> Result<()> {
        match key {
            "sub-font" => {
                let font = text.trim();
                if font.is_empty() || font.len() > 256 || font.chars().any(char::is_control) {
                    bail!("Enter a font family name.");
                }
                self.subtitles.font = font.into();
            }
            "sub-color" | "sub-background" => {
                let (r, g, b) = rgb(text)?;
                let color = format!("#{r:02X}{g:02X}{b:02X}");
                if key == "sub-color" {
                    self.subtitles.color = color;
                } else {
                    self.subtitles.background_color = color;
                }
            }
            _ => self.number(
                key,
                text.trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Enter a number."))?,
            )?,
        }
        Ok(())
    }

    pub fn flag(&mut self, key: &str, value: bool) -> Result<()> {
        match key {
            "preserve-pitch" => self.video.preserve_pitch = value,
            "eq-enabled" => self.audio.eq_enabled = value,
            "sub-background-enabled" => self.subtitles.background_enabled = value,
            "sub-override" => self.subtitles.override_style = value,
            _ => bail!("Unknown player setting."),
        }
        Ok(())
    }

    fn sanitize(&mut self) {
        for v in [
            &mut self.video.brightness,
            &mut self.video.contrast,
            &mut self.video.saturation,
            &mut self.video.gamma,
            &mut self.video.hue,
        ] {
            *v = bounded(*v, -100., 100., 0.);
        }
        self.video.speed = bounded(self.video.speed, 0.25, 4., 1.);
        self.audio.delay = bounded(self.audio.delay, -10., 10., 0.);
        for v in &mut self.audio.bands {
            *v = bounded(*v, -12., 12., 0.);
        }
        if self.audio.preset >= PRESETS.len() || self.audio.bands != PRESETS[self.audio.preset] {
            self.audio.preset = 4;
        }
        let sub = &mut self.subtitles;
        sub.scale = bounded(sub.scale, 0.5, 3., 1.);
        sub.position = bounded(sub.position, 0., 100., 100.);
        sub.delay = bounded(sub.delay, -3600., 3600., 0.);
        sub.background_opacity = bounded(sub.background_opacity, 0., 100., 75.);
        if rgb(&sub.color).is_err() {
            sub.color = "#FFFFFF".into();
        }
        if rgb(&sub.background_color).is_err() {
            sub.background_color = "#000000".into();
        }
        if sub.font.trim().is_empty()
            || sub.font.len() > 256
            || sub.font.chars().any(char::is_control)
        {
            sub.font = "sans-serif".into();
        }
    }
}

/// A single labelled graph can be replaced without disturbing other audio filters.
pub fn eq_filter(audio: &AudioSettings) -> String {
    // Conservative headroom for overlapping positive bands, plus 1 dB margin.
    let boost: f64 = audio.bands.iter().copied().filter(|v| *v > 0.).sum();
    let mut filters = vec![format!(
        "volume={}dB",
        if boost > 0. { -boost - 1. } else { 0. }
    )];
    for (frequency, gain) in FREQUENCIES.iter().zip(audio.bands) {
        filters.push(format!("equalizer=f={frequency}:t=o:w=1:g={gain}"));
    }
    format!("@putmpv_eq:lavfi=[{}]", filters.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_locked_sections_survive_restart() {
        let mut session = Session::default();
        session.values.number("speed", 1.5).unwrap();
        session.values.number("audio-delay", 0.25).unwrap();
        assert!(session.saved().is_empty());
        session.locked[0] = true;
        let restarted = Session::from_saved(session.saved());
        assert_eq!(restarted.values.video.speed, 1.5);
        assert_eq!(restarted.values.audio.delay, 0.);
        session.locked[0] = false;
        assert!(session.saved().is_empty());
        assert_eq!(session.values.video.speed, 1.5);
    }

    #[test]
    fn reset_keeps_lock_and_other_sections() {
        let mut session = Session {
            locked: [true, true, false],
            ..Session::default()
        };
        session.values.number("speed", 2.).unwrap();
        session.values.number("audio-delay", 1.).unwrap();
        session.reset(0);
        assert_eq!(session.saved().video.unwrap().speed, 1.);
        assert_eq!(session.values.audio.delay, 1.);
        assert!(session.locked[0]);
    }

    #[test]
    fn validation_and_color_order() {
        let mut settings = Settings::default();
        assert!(settings.number("speed", f64::NAN).is_err());
        settings.number("brightness", 200.).unwrap();
        assert_eq!(settings.video.brightness, 100.);
        assert!(settings.text("sub-color", "#abc").is_err());
        assert_eq!(mpv_color("#123456", 50.).unwrap(), "#80123456");
        assert!(settings.text("speed", "infinity").is_err());
    }

    #[test]
    fn pitch_toggle_preserves_old_defaults_and_resets_with_video() {
        let saved: SavedSettings = serde_json::from_str(r#"{"video":{"speed":1.5}}"#).unwrap();
        let mut session = Session::from_saved(saved);
        assert!(session.values.video.preserve_pitch);
        session.values.flag("preserve-pitch", false).unwrap();
        assert!(!session.saved().video.unwrap().preserve_pitch);
        session.reset(0);
        assert!(session.saved().video.unwrap().preserve_pitch);
    }

    #[test]
    fn new_presets_preserve_custom_ids_and_existing_saved_curves() {
        let custom = AudioSettings {
            preset: 4,
            bands: [3.; 10],
            ..AudioSettings::default()
        };
        let session = Session::from_saved(SavedSettings {
            audio: Some(custom.clone()),
            ..SavedSettings::default()
        });
        assert_eq!(session.values.audio, custom);
        let night = AudioSettings {
            preset: 5,
            bands: PRESETS[5],
            ..AudioSettings::default()
        };
        let session = Session::from_saved(SavedSettings {
            audio: Some(night.clone()),
            ..SavedSettings::default()
        });
        assert_eq!(session.values.audio, night);
        let old_vocal = AudioSettings {
            preset: 1,
            bands: [-4., -4., -3., -2., 0., 2., 4., 3., 1., 0.],
            ..AudioSettings::default()
        };
        let session = Session::from_saved(SavedSettings {
            audio: Some(old_vocal.clone()),
            ..SavedSettings::default()
        });
        assert_eq!(session.values.audio.bands, old_vocal.bands);
        assert_eq!(session.values.audio.preset, 4);
    }

    #[test]
    fn presets_and_custom_graph_are_labelled_and_bounded() {
        let mut settings = Settings::default();
        settings.audio.bands = PRESETS[2];
        let graph = eq_filter(&settings.audio);
        assert!(graph.starts_with("@putmpv_eq:lavfi=[volume=-18dB,"));
        assert_eq!(graph.matches("equalizer=").count(), 10);
        settings.number("eq-9", 15.).unwrap();
        assert_eq!(settings.audio.bands[9], 12.);
        assert_eq!(settings.audio.preset, 4);
    }

    #[test]
    fn old_config_and_invalid_saved_values() {
        let cfg: crate::storage::config::ConfigFile = serde_json::from_str("{}").unwrap();
        assert!(cfg.player_settings.is_empty());
        let saved: SavedSettings =
            serde_json::from_str(r##"{"video":{"speed":99},"subtitles":{"color":"bad"}}"##)
                .unwrap();
        let session = Session::from_saved(saved);
        assert_eq!(session.values.video.speed, 4.);
        assert_eq!(session.values.subtitles.color, "#FFFFFF");
        assert_eq!(session.locked, [true, false, true]);
    }
}
