// FBO + Texture helper for mpv rendering into a Slint OpenGL texture.
// Ported from maurges/slint-mpv-widget src/gl.rs (MIT licence).
use std::num::NonZeroU32;
use std::rc::Rc;

use glow::HasContext;

pub struct Texture {
    pub texture: glow::NativeTexture,
    pub fbo: glow::NativeFramebuffer,
    pub width: u32,
    pub height: u32,
    pub gl: Rc<glow::Context>,
}

impl Texture {
    pub unsafe fn new(gl: &Rc<glow::Context>, width: u32, height: u32) -> Self {
        let fbo = gl
            .create_framebuffer()
            .expect("unable to create framebuffer");

        let texture = gl.create_texture().expect("unable to create texture");

        let saved_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));

        let old_unpack_align = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        let old_unpack_row = gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH);
        let old_unpack_skip_px = gl.get_parameter_i32(glow::UNPACK_SKIP_PIXELS);
        let old_unpack_skip_rows = gl.get_parameter_i32(glow::UNPACK_SKIP_ROWS);

        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, width as i32);
        gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
        gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);

        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            None,
        );

        // Restore pixel store state
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, old_unpack_align);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, old_unpack_row);
        gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, old_unpack_skip_px);
        gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, old_unpack_skip_rows);

        // Attach texture to FBO
        let saved_fbo = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32;
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        debug_assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );

        // Restore bindings
        let saved_fbo_native = std::num::NonZeroU32::new(saved_fbo).map(glow::NativeFramebuffer);
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, saved_fbo_native);

        let saved_tex_native = std::num::NonZeroU32::new(saved_tex).map(glow::NativeTexture);
        gl.bind_texture(glow::TEXTURE_2D, saved_tex_native);

        Self {
            texture,
            fbo,
            width,
            height,
            gl: gl.clone(),
        }
    }

    /// Bind our FBO, run callback, then restore the previous FBO.
    pub unsafe fn with_texture_as_active_fbo<R>(&self, callback: impl FnOnce() -> R) -> R {
        let saved_fbo = self.gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32;
        self.gl
            .bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbo));
        let result = callback();
        let saved_native = std::num::NonZeroU32::new(saved_fbo).map(glow::NativeFramebuffer);
        self.gl
            .bind_framebuffer(glow::DRAW_FRAMEBUFFER, saved_native);
        result
    }

    pub fn raw_fbo_id(&self) -> i32 {
        // glow::NativeFramebuffer is a newtype over NonZeroU32
        // SAFETY: NativeFramebuffer is repr(transparent) NonZeroU32
        unsafe { std::mem::transmute::<glow::NativeFramebuffer, NonZeroU32>(self.fbo).get() as i32 }
    }

    pub fn raw_texture_id(&self) -> NonZeroU32 {
        // SAFETY: NativeTexture is repr(transparent) NonZeroU32
        unsafe { std::mem::transmute::<glow::NativeTexture, NonZeroU32>(self.texture) }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.texture);
        }
    }
}
