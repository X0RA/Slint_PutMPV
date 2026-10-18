mod anime4k;
mod anime4k_assets;
mod app;
mod gl;
#[cfg(target_os = "linux")]
mod linux_inhibitor;
mod media_controls;
mod mpv_nodes;
pub(crate) mod settings;
mod settings_ui;
mod sleep_inhibitor;

pub(crate) use app::refresh_watch_state_views;
pub use app::{EmbeddedPlayer, PlaybackQueueItem};

use glow::HasContext;
use std::ffi::{c_void, CStr, CString};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use libmpv2::{
    events::mpv_event_id,
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
    Format, Mpv,
};

type SlintGetProcAddress<'a> = dyn Fn(&CStr) -> *const c_void + 'a;

/// Slint and libmpv share this context, so select its API before any window is
/// created. Slint otherwise prefers GLES, whose mediump texture reads trigger
/// severe colour errors in Anime4K's CNN passes on NVIDIA drivers (mpv #18566).
/// Desktop GL keeps the existing FemtoVG/libmpv renderer and unmodified shaders.
pub(crate) fn select_graphics_backend() -> Result<()> {
    slint::BackendSelector::new().require_opengl().select()?;
    Ok(())
}

/// Type-erased handle to Slint's GL loader.
///
/// libmpv2 requires the `OpenGLInitParams` context to be `'static` because mpv
/// takes ownership of it, but Slint only lends us the loader for the duration of
/// the `RenderingSetup` callback. We hold it as a raw pointer and rely on mpv
/// resolving every GL function during `create_render_context`, which is the only
/// point at which the pointer is still live.
struct SlintGlProcAddress {
    get_proc_address: *const SlintGetProcAddress<'static>,
}

fn get_proc_address(ctx: &SlintGlProcAddress, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: only called by mpv while `create_render_context` is on the stack,
    // where the borrow from Slint is still valid. See the type's docs.
    let loader = unsafe { &*ctx.get_proc_address };
    loader(&name).cast_mut()
}

pub struct PlayerEngine {
    mpv: &'static Mpv,
    anime4k_blocked: AtomicBool,
}

impl PlayerEngine {
    pub fn new() -> Result<Self> {
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("vo", "libmpv")?;
            init.set_option("terminal", "no")?;
            Ok(())
        })
        .map_err(|e| anyhow!("{e}"))?;

        Ok(Self {
            mpv: Box::leak(Box::new(mpv)),
            anime4k_blocked: AtomicBool::new(false),
        })
    }

    pub fn anime4k_blocked(&self) -> bool {
        self.anime4k_blocked.load(Ordering::Relaxed)
    }

    pub fn mpv(&self) -> &'static Mpv {
        self.mpv
    }

    pub fn load(&self, url: &str) -> Result<()> {
        self.mpv
            .command("loadfile", &[url, "replace"])
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.mpv.command("stop", &[]).map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    pub fn set_pause(&self, paused: bool) -> Result<()> {
        self.mpv
            .set_property("pause", paused)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn seek(&self, seconds: f64) -> Result<()> {
        self.mpv
            .set_property("time-pos", seconds.max(0.0))
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        self.mpv
            .set_property("volume", volume.clamp(0.0, 100.0))
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn set_mute(&self, muted: bool) -> Result<()> {
        self.mpv
            .set_property("mute", muted)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn set_sub_visibility(&self, visible: bool) -> Result<()> {
        self.mpv
            .set_property("sub-visibility", visible)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn set_sid(&self, track_id: &str) -> Result<()> {
        let value = if track_id.is_empty() { "no" } else { track_id };
        self.mpv
            .set_property("sid", value)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn add_subtitle(&self, path: &std::path::Path, title: &str, language: &str) -> Result<()> {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow!("Invalid subtitle path"))?;
        self.mpv
            .command("sub-add", &[path, "cached", title, language])
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn set_aid(&self, track_id: &str) -> Result<()> {
        let value = if track_id.is_empty() { "no" } else { track_id };
        self.mpv
            .set_property("aid", value)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn create_event_client(&self) -> Result<Mpv> {
        self.mpv
            // Errored out in debug mode with this in there
            // .create_client(Some("putmpv_events"))
            .create_client(None)
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn observe_properties(event_client: &Mpv) -> Result<()> {
        mpv_nodes::request_errors(event_client)?;
        event_client
            .enable_event(mpv_event_id::FileLoaded)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .enable_event(mpv_event_id::AudioReconfig)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .enable_event(mpv_event_id::EndFile)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .enable_event(mpv_event_id::Seek)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("pause", Format::Flag, 1)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("time-pos", Format::Double, 2)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("duration", Format::Double, 3)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("volume", Format::Double, 4)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("mute", Format::Flag, 5)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("sub-text", Format::String, 6)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("sid", Format::String, 7)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("aid", Format::String, 8)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("track-list/count", Format::Int64, 9)
            .map_err(|e| anyhow!("{e}"))?;
        event_client
            .observe_property("demuxer-cache-time", Format::Double, 10)
            .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }
}

pub struct PlayerRenderer {
    gl: Rc<glow::Context>,
    texture: gl::Texture,
    texture_published: bool,
    render_context: RenderContext<'static>,
}

impl PlayerRenderer {
    pub fn new(
        engine: &PlayerEngine,
        gl: glow::Context,
        get_proc_address_loader: &SlintGetProcAddress<'_>,
    ) -> Result<Self> {
        // Slint/glutin already retry with default context attributes when the
        // desktop request fails. Inspect what was actually created, not merely
        // what was requested. GLES remains usable on non-NVIDIA systems.
        let version = unsafe { gl.get_parameter_string(glow::VERSION) };
        let vendor = unsafe { gl.get_parameter_string(glow::VENDOR) };
        let blocked = anime4k::incompatible_context(&version, &vendor);
        engine.anime4k_blocked.store(blocked, Ordering::Relaxed);
        if version.starts_with("OpenGL ES") {
            tracing::warn!(%version, %vendor, "Using an OpenGL ES context; Anime4K available: {}", !blocked);
        }
        let mpv = engine.mpv();
        let gl = Rc::new(gl);
        let texture = unsafe { gl::Texture::new(&gl, 320, 200) };
        // SAFETY: erases the loader's lifetime to satisfy libmpv2's `'static`
        // bound. mpv only invokes it during the `create_render_context` call
        // below, while the borrow is still live.
        let loader: *const SlintGetProcAddress<'static> = unsafe {
            std::mem::transmute::<*const SlintGetProcAddress<'_>, *const SlintGetProcAddress<'static>>(
                get_proc_address_loader as *const _,
            )
        };
        let render_context = mpv
            .create_render_context(vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(OpenGLInitParams {
                    get_proc_address,
                    ctx: SlintGlProcAddress {
                        get_proc_address: loader,
                    },
                }),
            ])
            .map_err(|e| anyhow!("{e}"))?;

        Ok(Self {
            gl,
            texture,
            texture_published: false,
            render_context,
        })
    }

    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        self.render_context.set_update_callback(callback);
    }

    pub fn render(&mut self, width: u32, height: u32) -> Result<Option<slint::Image>> {
        let width = width.max(1);
        let height = height.max(1);
        let recreated = if self.texture.width != width || self.texture.height != height {
            self.texture = unsafe { gl::Texture::new(&self.gl, width, height) };
            true
        } else {
            false
        };

        unsafe {
            self.texture.with_texture_as_active_fbo(|| {
                self.render_context
                    .render::<()>(
                        self.texture.raw_fbo_id(),
                        width as i32,
                        height as i32,
                        false,
                    )
                    .map_err(|e| anyhow!("{e}"))
            })?;
        }

        if recreated || !self.texture_published {
            self.texture_published = true;
            let texture = unsafe {
                slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    self.texture.raw_texture_id(),
                    (width, height).into(),
                )
                .build()
            };
            Ok(Some(texture))
        } else {
            Ok(None)
        }
    }
}
