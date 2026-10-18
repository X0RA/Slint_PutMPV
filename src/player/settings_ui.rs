//! Main-thread adapter for the settings panel and libmpv.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

use super::{
    anime4k::{self, Quality, Shaders},
    settings::{self, Session, Settings, PRESETS},
    PlayerEngine,
};
use crate::{storage::config::ConfigStore, AppWindow, PlayerSettingsValues};

pub(crate) struct Controller {
    app: slint::Weak<AppWindow>,
    engine: Option<Arc<PlayerEngine>>,
    config: Arc<ConfigStore>,
    session: RefCell<Session>,
    applied: RefCell<Settings>,
    apply_timer: Timer,
    save_timer: Timer,
    dirty: Cell<bool>,
    eq_installed: Cell<bool>,
    anime4k: Shaders,
    enhancements: super::enhancements::Shaders,
    diagnostics_timer: Timer,
    anime4k_status: RefCell<String>,
    anime4k_warning: Cell<bool>,
    anime4k_failure: RefCell<Option<String>>,
    dropped_frames: Cell<Option<i64>>,
}

impl Controller {
    pub fn install(
        app: &AppWindow,
        engine: Option<Arc<PlayerEngine>>,
        config: Arc<ConfigStore>,
    ) -> Rc<Self> {
        let session = Session::from_saved(config.player_settings());
        let this = Rc::new(Self {
            anime4k: Shaders::new(config.path().parent().expect("config has a directory")),
            enhancements: super::enhancements::Shaders::new(
                config.path().parent().expect("config has a directory"),
            ),
            app: app.as_weak(),
            engine,
            config,
            applied: RefCell::new(Settings::default()),
            session: RefCell::new(session),
            apply_timer: Timer::default(),
            save_timer: Timer::default(),
            dirty: Cell::new(false),
            eq_installed: Cell::new(false),
            diagnostics_timer: Timer::default(),
            anime4k_status: RefCell::new(String::new()),
            anime4k_warning: Cell::new(false),
            anime4k_failure: RefCell::new(None),
            dropped_frames: Cell::new(None),
        });
        this.publish();
        this.restore();
        let c = this.clone();
        app.on_player_setting_number(move |key, value| c.edit(|s| s.number(&key, value as f64)));
        let c = this.clone();
        app.on_player_setting_text(move |key, value| c.edit(|s| s.text(&key, &value)));
        let c = this.clone();
        app.on_player_setting_flag(move |key, value| c.edit(|s| s.flag(&key, value)));
        let c = this.clone();
        app.on_player_setting_preset(move |index| {
            c.edit(|s| {
                if let Some(bands) = usize::try_from(index)
                    .ok()
                    .and_then(|i| PRESETS.get(i))
                    .filter(|_| index != 4)
                {
                    s.audio.bands = *bands;
                    s.audio.preset = index as usize;
                } else if index == 4 {
                    s.audio.preset = 4;
                }
                Ok(())
            })
        });
        let c = this.clone();
        app.on_player_settings_lock(move |section| c.toggle_lock(section));
        let c = this.clone();
        app.on_player_settings_reset(move |section| {
            if !(0..4).contains(&section) {
                return;
            }
            c.session.borrow_mut().reset(section as usize);
            if section == 3 {
                c.anime4k_failure.borrow_mut().take();
                c.anime4k_status.borrow_mut().clear();
                c.anime4k_warning.set(false);
            }
            c.publish();
            c.finish_edit();
        });
        let c = this.clone();
        app.on_player_settings_flush(move || c.flush().is_ok());
        let c = this.clone();
        app.on_player_settings_restore(move || c.restore());
        let c = this.clone();
        app.on_player_shader_error(move |message| c.shader_failed(&message));
        let weak = Rc::downgrade(&this);
        this.diagnostics_timer
            .start(TimerMode::Repeated, Duration::from_secs(2), move || {
                if let Some(c) = weak.upgrade() {
                    c.update_diagnostics();
                }
            });
        this
    }

    fn report(&self, result: Result<()>) -> bool {
        if let Some(app) = self.app.upgrade() {
            app.set_player_settings_error(match &result {
                Ok(()) => "".into(),
                Err(error) => format!("{error:#}").into(),
            });
        }
        if let Err(error) = &result {
            tracing::warn!("player settings: {error:#}");
        }
        result.is_ok()
    }

    fn publish(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let session = self.session.borrow();
        let v = &session.values.video;
        let a = &session.values.audio;
        let s = &session.values.subtitles;
        let color = |text: &str, opacity: f64| {
            let (r, g, b) = settings::rgb(text).unwrap_or((255, 255, 255));
            slint::Color::from_argb_u8((opacity / 100. * 255.).round() as u8, r, g, b)
        };
        app.set_player_settings_values(PlayerSettingsValues {
            upscaler: v.enhancements.upscaler as i32,
            refinement: v.enhancements.refinement,
            chroma: v.enhancements.chroma,
            downscale: v.enhancements.downscale,
            film_grain: i32::from(v.enhancements.grain),
            anime4k_enabled: v.anime4k.enabled,
            anime4k_mode: v.anime4k.mode as i32,
            anime4k_hq: v.anime4k.quality == Quality::Hq,
            anime4k_status: self.anime4k_status.borrow().as_str().into(),
            anime4k_warning: self.anime4k_warning.get(),
            brightness: v.brightness as f32,
            contrast: v.contrast as f32,
            saturation: v.saturation as f32,
            gamma: v.gamma as f32,
            hue: v.hue as f32,
            speed: v.speed as f32,
            preserve_pitch: v.preserve_pitch,
            audio_delay: a.delay as f32,
            eq_enabled: a.eq_enabled,
            eq_preset: a.preset as i32,
            bands: ModelRc::new(VecModel::from(
                a.bands.iter().map(|v| *v as f32).collect::<Vec<_>>(),
            )),
            sub_scale: s.scale as f32,
            sub_font: s.font.as_str().into(),
            sub_color_hex: s.color.as_str().into(),
            sub_color: color(&s.color, 100.),
            sub_background_enabled: s.background_enabled,
            sub_background_hex: s.background_color.as_str().into(),
            sub_background: color(&s.background_color, s.background_opacity),
            sub_opacity: s.background_opacity as f32,
            sub_position: s.position as f32,
            sub_delay: s.delay as f32,
            sub_override: s.override_style,
            video_locked: session.locked[0],
            audio_locked: session.locked[1],
            subtitles_locked: session.locked[2],
        });
    }

    fn edit(self: &Rc<Self>, edit: impl FnOnce(&mut Settings) -> Result<()>) {
        let result = edit(&mut self.session.borrow_mut().values);
        if self.report(result) {
            self.publish();
            self.finish_edit();
        }
    }

    fn finish_edit(self: &Rc<Self>) {
        // Throttle engine updates while dragging; do not wait until the drag ends.
        if self.apply_timer.running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.apply_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(80),
            move || {
                if let Some(c) = weak.upgrade() {
                    if c.apply_pending() {
                        let weak = Rc::downgrade(&c);
                        c.save_timer.start(
                            TimerMode::SingleShot,
                            Duration::from_millis(500),
                            move || {
                                if let Some(c) = weak.upgrade() {
                                    let _ = c.save();
                                }
                            },
                        );
                    }
                }
            },
        );
    }

    fn apply_pending(&self) -> bool {
        self.apply_timer.stop();
        let next = self.session.borrow().values.clone();
        let old = self.applied.borrow().clone();
        if next == old {
            self.dirty
                .set(self.session.borrow().saved() != self.config.player_settings());
            return true;
        }
        let result = self.apply(&next, Some(&old));
        if result.is_err() {
            // Roll back partial property writes and the displayed controls together.
            if let Err(error) = self.apply(&old, None) {
                tracing::warn!("player settings rollback: {error:#}");
            }
            self.session.borrow_mut().values = old;
            self.publish();
            return self.report(result);
        }
        *self.applied.borrow_mut() = next;
        self.publish();
        self.dirty
            .set(self.session.borrow().saved() != self.config.player_settings());
        self.report(Ok(()))
    }

    fn toggle_lock(&self, section: i32) {
        if !(0..3).contains(&section) || !self.apply_pending() {
            return;
        }
        let index = section as usize;
        let old = self.session.borrow().locked[index];
        self.session.borrow_mut().locked[index] = !old;
        self.dirty.set(true);
        if self.save().is_err() {
            self.session.borrow_mut().locked[index] = old;
            self.dirty
                .set(self.session.borrow().saved() != self.config.player_settings());
        }
        self.publish();
    }

    fn save(&self) -> Result<()> {
        self.save_timer.stop();
        if !self.dirty.get() {
            return Ok(());
        }
        // A save timer may fire while a newer slider edit is still queued.
        // Persist accepted preferences, never pending edits. The Anime4K
        // preference is retained even when the current context cannot run it.
        let mut snapshot = self.session.borrow().clone();
        snapshot.values = self.applied.borrow().clone();
        let result = self.config.set_player_settings(snapshot.saved()).context(
            "Could not save player settings. Your changes are still active for this session",
        );
        match result {
            Ok(()) => {
                self.dirty.set(false);
                self.report(Ok(()));
                Ok(())
            }
            Err(error) => {
                self.report(Err(anyhow::anyhow!("{error:#}")));
                Err(error)
            }
        }
    }

    pub fn flush(&self) -> Result<()> {
        if !self.apply_pending() {
            anyhow::bail!("Could not apply player settings");
        }
        self.save()
    }

    fn restore(&self) {
        let next = self.session.borrow().values.clone();
        if self.report(self.apply(&next, None)) {
            *self.applied.borrow_mut() = next;
        }
        self.publish();
    }

    fn shader_failed(&self, message: &str) {
        if self.anime4k_failure.borrow().is_some()
            || !(self.applied.borrow().video.anime4k.enabled
                || self.applied.borrow().video.enhancements.has_shaders())
            || self.engine.as_ref().is_some_and(|e| e.anime4k_blocked())
        {
            return;
        }
        tracing::warn!("Video shader error: {}", message.trim());
        // mpv compilation messages do not reliably identify the responsible
        // shader. Never disable Anime4K or rewrite saved preferences on a guess.
        self.anime4k.invalidate();
        self.enhancements.invalidate();
        let message = "A video effect failed to load. Try turning off an enhancement, or any custom shaders you have added.";
        *self.anime4k_failure.borrow_mut() = Some(message.into());
        *self.anime4k_status.borrow_mut() = message.into();
        self.anime4k_warning.set(true);
        self.publish();
    }

    fn update_diagnostics(&self) {
        if self.anime4k_failure.borrow().is_some()
            || !(self.applied.borrow().video.anime4k.enabled
                || self.applied.borrow().video.enhancements.has_shaders())
            || self.engine.as_ref().is_some_and(|e| e.anime4k_blocked())
        {
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let mpv = engine.mpv();
        let (message, warning) = if mpv.get_property::<i64>("video-params/w").unwrap_or(0) == 0 {
            self.dropped_frames.set(None);
            ("Starts when a video plays".into(), false)
        } else {
            let drops = mpv.get_property::<i64>("frame-drop-count").ok();
            let previous = self.dropped_frames.replace(drops);
            let dropping = !mpv.get_property::<bool>("pause").unwrap_or(true)
                && drops
                    .zip(previous)
                    .is_some_and(|(now, old)| now.saturating_sub(old) >= 3);
            if dropping {
                (
                    "Playback is stuttering. Try Lightweight or Standard, or turn off some extras."
                        .into(),
                    true,
                )
            } else {
                ("On".into(), false)
            }
        };
        if *self.anime4k_status.borrow() != message || self.anime4k_warning.get() != warning {
            *self.anime4k_status.borrow_mut() = message;
            self.anime4k_warning.set(warning);
            self.publish();
        }
    }

    fn apply(&self, next: &Settings, old: Option<&Settings>) -> Result<()> {
        let engine = self
            .engine
            .as_ref()
            .context("Embedded player is unavailable")?;
        let mpv = engine.mpv();
        let set_number = |key, value: f64| {
            mpv.set_property(key, value)
                .with_context(|| format!("Cannot adjust {key}"))
        };
        let set_text = |key, value: &str| {
            mpv.set_property(key, value)
                .with_context(|| format!("Cannot adjust {key}"))
        };
        let v = &next.video;
        let ov = old.map(|s| &s.video);
        for (key, value, previous) in [
            ("brightness", v.brightness, ov.map(|s| s.brightness)),
            ("contrast", v.contrast, ov.map(|s| s.contrast)),
            ("saturation", v.saturation, ov.map(|s| s.saturation)),
            ("gamma", v.gamma, ov.map(|s| s.gamma)),
            ("hue", v.hue, ov.map(|s| s.hue)),
            ("speed", v.speed, ov.map(|s| s.speed)),
            ("audio-delay", next.audio.delay, old.map(|s| s.audio.delay)),
            (
                "sub-scale",
                next.subtitles.scale,
                old.map(|s| s.subtitles.scale),
            ),
            (
                "sub-pos",
                next.subtitles.position,
                old.map(|s| s.subtitles.position),
            ),
            (
                "sub-delay",
                next.subtitles.delay,
                old.map(|s| s.subtitles.delay),
            ),
        ] {
            if previous != Some(value) {
                set_number(key, value)?;
            }
        }
        if old.is_none_or(|o| o.video.preserve_pitch != v.preserve_pitch) {
            mpv.set_property("audio-pitch-correction", v.preserve_pitch)
                .context("Cannot adjust pitch preservation")?;
        }
        let s = &next.subtitles;
        if old.is_none_or(|o| o.subtitles.font != s.font) {
            set_text("sub-font", &s.font)?;
        }
        if old.is_none_or(|o| o.subtitles.color != s.color) {
            set_text("sub-color", &settings::mpv_color(&s.color, 100.)?)?;
        }
        if old.is_none_or(|o| o.subtitles.override_style != s.override_style) {
            set_text(
                "sub-ass-override",
                if s.override_style { "force" } else { "scale" },
            )?;
        }
        if old.is_none_or(|o| {
            o.subtitles.background_enabled != s.background_enabled
                || o.subtitles.background_color != s.background_color
                || o.subtitles.background_opacity != s.background_opacity
        }) {
            set_text(
                "sub-border-style",
                if s.background_enabled {
                    "background-box"
                } else {
                    "outline-and-shadow"
                },
            )?;
            set_text(
                "sub-back-color",
                &settings::mpv_color(
                    &s.background_color,
                    if s.background_enabled {
                        s.background_opacity
                    } else {
                        0.
                    },
                )?,
            )?;
        }
        if old.is_none_or(|o| {
            o.audio.eq_enabled != next.audio.eq_enabled || o.audio.bands != next.audio.bands
        }) {
            if next.audio.eq_enabled {
                mpv.command("af", &["add", &settings::eq_filter(&next.audio)])
                    .context("The audio equalizer is unavailable in this mpv build")?;
                self.eq_installed.set(true);
            } else if self.eq_installed.get() {
                mpv.command("af", &["remove", "@putmpv_eq"])
                    .context("Could not disable the equalizer")?;
                self.eq_installed.set(false);
            }
        }
        // Last, so failures in unrelated settings cannot leave a newly selected
        // shader chain partially accepted. Reconcile again on every file load.
        if old.is_none_or(|o| {
            o.video.anime4k != next.video.anime4k || o.video.enhancements != v.enhancements
        }) {
            // Keep requested/persisted settings separate from the effective
            // shader chain. A hardware restriction must not turn a remembered
            // preference off when an unrelated setting is saved.
            let mut effective = next.video.anime4k.clone();
            let unavailable =
                (effective.enabled || v.enhancements.has_shaders()) && engine.anime4k_blocked();
            effective.enabled &= !unavailable;
            let enhancements = if unavailable {
                v.enhancements.without_shaders()
            } else {
                v.enhancements.clone()
            };
            self.enhancements
                .apply(mpv, &self.anime4k, &effective, &enhancements)?;
            self.dropped_frames.set(None);
            // FileLoaded and rollback also call apply(None): neither should
            // erase a failure. Only an accepted Anime4K edit (or Reset) clears it.
            if old.is_some_and(|o| {
                o.video.anime4k != next.video.anime4k || o.video.enhancements != v.enhancements
            }) {
                self.anime4k_failure.borrow_mut().take();
            }
            let failure = if unavailable {
                Some(if v.enhancements.has_shaders() {
                    "AI detail, Anime4K and the extras can't run with your graphics driver right now, so normal scaling is used. Your choices are saved.".into()
                } else {
                    anime4k::GLES_WARNING.into()
                })
            } else {
                self.anime4k_failure.borrow().clone()
            };
            self.anime4k_warning.set(failure.is_some());
            *self.anime4k_status.borrow_mut() = failure.unwrap_or_else(|| {
                if next.video.anime4k.enabled || v.enhancements.has_shaders() {
                    "Starts when a video plays".into()
                } else {
                    String::new()
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a desktop for the Slint settings controller"]
    fn shader_warning_survives_restore_without_changing_selection() {
        crate::player::select_graphics_backend().unwrap();
        let app = AppWindow::new().unwrap();
        let base =
            std::env::temp_dir().join(format!("putmpv-shader-warning-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let config = Arc::new(ConfigStore::for_test(base.join("config.json")));
        let engine = Arc::new(PlayerEngine::new().unwrap());
        let custom = vec!["/custom/broken.glsl".to_owned()];
        super::super::mpv_nodes::set_strings(engine.mpv(), "glsl-shaders", &custom).unwrap();
        let controller = Controller::install(&app, Some(engine.clone()), config.clone());
        app.invoke_player_setting_flag("anime4k-enabled".into(), true);
        controller.flush().unwrap();
        app.invoke_player_settings_lock(0);
        let saved = config.player_settings();
        let chain = super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap();
        controller.shader_failed("Shader compilation failed in custom shader");
        let warning = app.get_player_settings_values().anime4k_status;
        // Holding this borrow also proves repeated source-log lines never try
        // to rewrite the failure state (which would panic on a mutable borrow).
        {
            let _failure = controller.anime4k_failure.borrow();
            for _ in 0..40 {
                controller.shader_failed("vec4 hook() { HOOKED_tex(HOOKED_pos); }");
            }
        }

        assert!(app.get_player_settings_values().anime4k_enabled);
        controller.restore();
        controller.update_diagnostics();
        assert_eq!(app.get_player_settings_values().anime4k_status, warning);
        assert!(app.get_player_settings_values().anime4k_warning);
        assert_eq!(config.player_settings(), saved);
        assert_eq!(
            super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
            chain
        );
        app.invoke_player_settings_reset(0);
        controller.flush().unwrap();
        assert_eq!(app.get_player_settings_values().anime4k_status, warning);
        assert!(app.get_player_settings_values().anime4k_enabled);
        app.invoke_player_setting_number("brightness".into(), 10.);
        controller.flush().unwrap();
        assert_eq!(app.get_player_settings_values().anime4k_status, warning);
        app.invoke_player_setting_number("anime4k-mode".into(), 1.);
        controller.flush().unwrap();
        assert!(!app.get_player_settings_values().anime4k_warning);
        controller.shader_failed("GLSL compilation failed");
        // Remembered settings survive an incompatible context and unrelated
        // video saves, then become effective again when desktop GL is available.
        engine
            .anime4k_blocked
            .store(true, std::sync::atomic::Ordering::Relaxed);
        controller.restore();
        assert!(app.get_player_settings_values().anime4k_enabled);
        assert_eq!(
            app.get_player_settings_values().anime4k_status,
            anime4k::GLES_WARNING
        );
        assert_eq!(
            super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
            custom
        );
        app.invoke_player_setting_number("contrast".into(), 12.);
        controller.flush().unwrap();
        assert!(
            config
                .player_settings()
                .video
                .as_ref()
                .unwrap()
                .anime4k
                .enabled
        );
        assert_eq!(
            config.player_settings().video.as_ref().unwrap().contrast,
            12.
        );
        // An explicit Off while blocked is still a real preference change.
        app.invoke_player_setting_flag("anime4k-enabled".into(), false);
        controller.flush().unwrap();
        assert!(
            !config
                .player_settings()
                .video
                .as_ref()
                .unwrap()
                .anime4k
                .enabled
        );
        app.invoke_player_setting_flag("anime4k-enabled".into(), true);
        controller.flush().unwrap();
        assert!(
            config
                .player_settings()
                .video
                .as_ref()
                .unwrap()
                .anime4k
                .enabled
        );
        assert_eq!(
            super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
            custom
        );
        engine
            .anime4k_blocked
            .store(false, std::sync::atomic::Ordering::Relaxed);
        controller.restore();
        assert!(app.get_player_settings_values().anime4k_enabled);
        assert!(!app.get_player_settings_values().anime4k_warning);
        assert!(
            super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders")
                .unwrap()
                .len()
                > custom.len()
        );
        app.invoke_player_settings_reset(3);
        controller.flush().unwrap();
        assert!(!app.get_player_settings_values().anime4k_warning);
        assert!(!app.get_player_settings_values().anime4k_enabled);
        assert_eq!(
            super::super::mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
            custom
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
