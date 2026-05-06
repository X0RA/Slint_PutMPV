mod app;
mod gl;

pub use app::{EmbeddedPlayer, PlaybackQueueItem};

use std::ffi::{c_void, CStr, CString};
use std::rc::Rc;

use anyhow::{anyhow, Result};
use libmpv2::{
    events::mpv_event_id,
    render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType},
    Format, Mpv,
};

type SlintGetProcAddress<'a> = dyn Fn(&CStr) -> *const c_void + 'a;

struct SlintGlProcAddress<'a> {
    get_proc_address: &'a SlintGetProcAddress<'a>,
}

fn get_proc_address(ctx: &SlintGlProcAddress<'_>, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    (ctx.get_proc_address)(&name).cast_mut()
}

pub struct PlayerEngine {
    mpv: &'static Mpv,
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
        })
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
    render_context: RenderContext,
}

impl PlayerRenderer {
    pub fn new(
        mpv: &Mpv,
        gl: glow::Context,
        get_proc_address_loader: &SlintGetProcAddress<'_>,
    ) -> Result<Self> {
        let gl = Rc::new(gl);
        let texture = unsafe { gl::Texture::new(&gl, 320, 200) };
        let render_context = unsafe {
            RenderContext::new(
                &mut *mpv.ctx.as_ptr(),
                vec![
                    RenderParam::ApiType(RenderParamApiType::OpenGl),
                    RenderParam::InitParams(OpenGLInitParams {
                        get_proc_address,
                        ctx: SlintGlProcAddress {
                            get_proc_address: get_proc_address_loader,
                        },
                    }),
                ],
            )
        }
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
