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
}

impl Controller {
    pub fn install(
        app: &AppWindow,
        engine: Option<Arc<PlayerEngine>>,
        config: Arc<ConfigStore>,
    ) -> Rc<Self> {
        let session = Session::from_saved(config.player_settings());
        let this = Rc::new(Self {
            app: app.as_weak(),
            engine,
            config,
            applied: RefCell::new(Settings::default()),
            session: RefCell::new(session),
            apply_timer: Timer::default(),
            save_timer: Timer::default(),
            dirty: Cell::new(false),
            eq_installed: Cell::new(false),
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
            if !(0..3).contains(&section) {
                return;
            }
            c.session.borrow_mut().reset(section as usize);
            c.publish();
            c.finish_edit();
        });
        let c = this.clone();
        app.on_player_settings_flush(move || c.flush().is_ok());
        let c = this.clone();
        app.on_player_settings_restore(move || c.restore());
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
        // Persist only values that mpv has accepted, never pending edits.
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
        Ok(())
    }
}
