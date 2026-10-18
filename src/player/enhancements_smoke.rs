//! Opt-in check of spatial shader compilation, actual pass execution and timing.
use super::*;
use crate::player::{
    anime4k,
    enhancements::{self, Settings, Upscaler},
    mpv_nodes,
};
use std::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

fn case(index: usize) -> (Settings, anime4k::Settings, bool) {
    let mut settings = Settings::default();
    let mut anime = anime4k::Settings::default();
    match index {
        1 => settings.upscaler = Upscaler::Lightweight,
        2 => settings.upscaler = Upscaler::FsrcnnxBalanced,
        3 => settings.upscaler = Upscaler::FsrcnnxHigh,
        4 => settings.refinement = true,
        5 => settings.chroma = true,
        6 => settings.downscale = true,
        7..=9 => {
            settings.refinement = true;
            settings.chroma = true;
            settings.downscale = true;
            settings.upscaler = if index == 8 {
                Upscaler::FsrcnnxHigh
            } else if index == 7 {
                Upscaler::FsrcnnxBalanced
            } else {
                Upscaler::Standard
            };
            anime.enabled = index == 9;
        }
        _ => (),
    }
    (settings, anime, index == 6)
}

#[test]
#[ignore = "requires an OpenGL desktop and PUTMPV_ENHANCEMENTS_SAMPLE video"]
fn enhancements_gpu_smoke() {
    let _ = tracing_subscriber::fmt().try_init();
    let sample = std::env::var("PUTMPV_ENHANCEMENTS_SAMPLE").expect("set a local test video path");
    crate::player::select_graphics_backend().unwrap();
    let app = AppWindow::new().unwrap();
    let engine = Arc::new(PlayerEngine::new().unwrap());
    let events = engine.create_event_client().unwrap();
    // This renderer intentionally does not use advanced-control mode, so
    // vo-passes is unavailable. Trace pass reports expose the same GPU timings
    // without changing render ownership or blocking the rendering thread.
    engine
        .mpv()
        .set_property("msg-level", "all=no,vo/libmpv=trace,libmpv_render=trace")
        .unwrap();
    // SAFETY: live client and static C string; mpv retains neither pointer.
    assert!(
        unsafe { libmpv2_sys::mpv_request_log_messages(events.ctx.as_ptr(), c"trace".as_ptr()) }
            >= 0
    );
    let observations = Arc::new(Mutex::new(vec![Vec::<(usize, f64)>::new(); 11]));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let current_stage = Arc::new(Mutex::new((0usize, Instant::now())));
    let done = Arc::new(AtomicBool::new(false));
    let reader = std::thread::spawn({
        let observations = observations.clone();
        let errors = errors.clone();
        let current_stage = current_stage.clone();
        let done = done.clone();
        move || {
            let mut passes = 0usize;
            let mut active = 0usize;
            let mut total_us = 0f64;
            while !done.load(Ordering::Relaxed) {
                if let Some(Ok(Event::LogMessage {
                    prefix,
                    level,
                    text,
                    ..
                })) = events.wait_event(0.1)
                {
                    if matches!(level, "error" | "fatal")
                        && (prefix.starts_with("vo/") || prefix.starts_with("libmpv_render"))
                    {
                        errors.lock().unwrap().push(text.to_owned());
                    }
                    if text.starts_with("pass '") {
                        passes += 1;
                        active += usize::from(
                            [
                                "Anime4K",
                                "feature map",
                                "mapping",
                                "sub-pixel",
                                "Chroma From Luma",
                                "SSSR",
                                "SSimDownscaler",
                            ]
                            .iter()
                            .any(|name| text.contains(name)),
                        );
                        total_us += text
                            .split(" avg ")
                            .nth(1)
                            .and_then(|s| s.split("us").next())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.);
                    } else if passes > 0 {
                        let (stage, since) = *current_stage.lock().unwrap();
                        // Ignore initial compilation and the transition between chains.
                        if since.elapsed().as_secs_f64() >= 1. {
                            observations.lock().unwrap()[stage].push((active, total_us / 1000.));
                        }
                        passes = 0;
                        active = 0;
                        total_us = 0.;
                    }
                }
            }
        }
    });
    let base = std::env::temp_dir().join(format!("putmpv-enhancements-gpu-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let custom = base.join("custom shader;with:punctuation.glsl");
    std::fs::write(&custom, "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC PutMPV preservation check\nvec4 hook() { return HOOKED_tex(HOOKED_pos); }\n").unwrap();
    let custom = vec![custom.to_str().unwrap().to_owned()];
    mpv_nodes::set_strings(engine.mpv(), "glsl-shaders", &custom).unwrap();
    engine.mpv().set_property("loop-file", "inf").unwrap();
    engine.mpv().set_property("mute", true).unwrap();
    app.set_view(crate::app_ui::VIEW_PLAYER);
    // A deterministic 2x output for the 640x360 sample also exercises the
    // conditional CNN upscalers on desktops that tile the window below 720p.
    app.set_player_texture_width(1280.);
    app.set_player_texture_height(720.);
    register_renderer(&app, engine.clone(), crate::app_ui::VIEW_PLAYER);
    // Match normal application startup: Slint must create the GL render context
    // before libmpv tries to initialize video output for the first file.
    slint::Timer::single_shot(std::time::Duration::from_millis(500), {
        let engine = engine.clone();
        move || engine.load(&sample).unwrap()
    });
    let shaders = anime4k::Shaders::new(&base);
    let enhancements = enhancements::Shaders::new(&base);
    let stage = Rc::new(Cell::new(0usize));
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(3),
        {
            let stage = stage.clone();
            let current_stage = current_stage.clone();
            let engine = engine.clone();
            move || {
                let index = stage.get();
                if index >= 10 {
                    assert_eq!(
                        mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
                        custom
                    );
                    engine.stop().unwrap();
                    if let Some(app) = weak.upgrade() {
                        app.hide().unwrap();
                    }
                    slint::quit_event_loop().unwrap();
                    return;
                }
                let (next, anime, downscale) = case(index + 1);
                if let Some(app) = weak.upgrade() {
                    app.set_player_texture_width(if downscale { 320. } else { 1280. });
                    app.set_player_texture_height(if downscale { 180. } else { 720. });
                }
                enhancements
                    .apply(engine.mpv(), &shaders, &anime, &next)
                    .unwrap();
                assert_eq!(
                    mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap()[0],
                    custom[0]
                );
                stage.set(index + 1);
                *current_stage.lock().unwrap() = (index + 1, Instant::now());
            }
        },
    );
    app.run().unwrap();
    timer.stop();
    done.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    let errors = errors.lock().unwrap();
    assert!(errors.is_empty(), "shader errors: {errors:?}");
    let observations = observations.lock().unwrap();
    for (index, samples) in observations.iter().enumerate() {
        assert!(
            !samples.is_empty(),
            "no render-pass reports at stage {index}"
        );
        let active = samples.iter().map(|s| s.0).max().unwrap();
        let mut timings: Vec<_> = samples
            .iter()
            .filter(|s| s.0 == active)
            .map(|s| s.1)
            .collect();
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "Enhancement stage {index}: {active} active passes; median GPU frame {:.2} ms",
            timings[timings.len() / 2]
        );
        if (2..10).contains(&index) {
            assert!(
                active > 0,
                "Enhancement passes did not run at stage {index}"
            );
        } else {
            assert_eq!(active, 0);
        }
    }
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
#[ignore = "requires an OpenGL desktop"]
fn enhancements_colour_smoke() {
    use glow::HasContext;

    match std::env::var("PUTMPV_TEST_GL").as_deref() {
        Ok("es") => slint::BackendSelector::new()
            .require_opengl_es()
            .select()
            .unwrap(),
        _ => crate::player::select_graphics_backend().unwrap(),
    }
    let base =
        std::env::temp_dir().join(format!("putmpv-enhancements-colour-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let sample = base.join("greys.ppm");
    let shades = [32u8, 80, 128, 176, 224];
    let mut ppm = b"P6\n320 180\n255\n".to_vec();
    for _ in 0..180 {
        for x in 0..320 {
            ppm.extend_from_slice(&[shades[x / 64]; 3]);
        }
    }
    std::fs::write(&sample, ppm).unwrap();
    // Neural luma and chroma hooks require a real subsampled YUV fixture.
    let yuv = base.join("greys.mkv");
    assert!(std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-loop", "1", "-i"])
        .arg(&sample)
        .args(["-t", "2", "-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&yuv)
        .status()
        .unwrap()
        .success());
    let sample = yuv;
    let app = AppWindow::new().unwrap();
    let engine = Arc::new(PlayerEngine::new().unwrap());
    engine
        .mpv()
        .set_property("image-display-duration", "inf")
        .unwrap();
    engine.mpv().set_property("pause", true).unwrap();
    engine.mpv().set_property("loop-file", "inf").unwrap();
    app.set_view(crate::app_ui::VIEW_PLAYER);
    let stage = Rc::new(Cell::new(0usize));
    let since = Rc::new(Cell::new(Instant::now()));
    let frames = Rc::new(RefCell::new(vec![None; 11]));
    let weak = app.as_weak();
    let mut renderer: Option<PlayerRenderer> = None;
    app.window()
        .set_rendering_notifier({
            let engine = engine.clone();
            let stage = stage.clone();
            let since = since.clone();
            let frames = frames.clone();
            let base = base.clone();
            move |state, api| match state {
                slint::RenderingState::RenderingSetup => {
                    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                        panic!("OpenGL required")
                    };
                    let gl = unsafe {
                        glow::Context::from_loader_function_cstr(|name| get_proc_address(name))
                    };
                    eprintln!("Colour check GL: {}", unsafe {
                        gl.get_parameter_string(glow::VERSION)
                    });
                    renderer = Some(PlayerRenderer::new(&engine, gl, get_proc_address).unwrap());
                    // Loading before GL setup finishes can make libmpv reject
                    // video output on a cold driver/shader cache.
                    since.set(Instant::now());
                    slint::Timer::single_shot(std::time::Duration::ZERO, {
                        let engine = engine.clone();
                        let sample = sample.clone();
                        move || engine.load(sample.to_str().unwrap()).unwrap()
                    });
                }
                slint::RenderingState::BeforeRendering => {
                    let (Some(renderer), Some(app)) = (renderer.as_mut(), weak.upgrade()) else {
                        return;
                    };
                    if let Some(image) = renderer.render(640, 360).unwrap() {
                        app.set_player_texture(image);
                    }
                    let index = stage.get();
                    if since.get().elapsed().as_millis() < 800 || frames.borrow()[index].is_some() {
                        return;
                    }
                    let mut pixels = vec![0u8; 640 * 360 * 4];
                    unsafe {
                        let gl = &renderer.gl;
                        let previous = std::num::NonZeroU32::new(
                            gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING) as u32,
                        )
                        .map(glow::NativeFramebuffer);
                        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(renderer.texture.fbo));
                        gl.read_pixels(
                            0,
                            0,
                            640,
                            360,
                            glow::RGBA,
                            glow::UNSIGNED_BYTE,
                            glow::PixelPackData::Slice(Some(&mut pixels)),
                        );
                        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, previous);
                    }
                    let mut averages = [[0f64; 3]; 5];
                    for (patch, average) in averages.iter_mut().enumerate() {
                        for y in 100..260 {
                            for x in (patch * 128 + 32)..(patch * 128 + 96) {
                                for (channel, value) in average.iter_mut().enumerate() {
                                    *value += f64::from(pixels[(y * 640 + x) * 4 + channel])
                                        / (160. * 64.);
                                }
                            }
                        }
                    }
                    let mut ppm = b"P6\n640 360\n255\n".to_vec();
                    for pixel in pixels.as_chunks::<4>().0 {
                        ppm.extend_from_slice(&pixel[..3]);
                    }
                    std::fs::write(base.join(format!("stage-{index}.ppm")), ppm).unwrap();
                    eprintln!("Colour stage {index}: {averages:.2?}");
                    frames.borrow_mut()[index] = Some(averages);
                }
                slint::RenderingState::RenderingTeardown => renderer = None,
                _ => {}
            }
        })
        .unwrap();
    let shaders = anime4k::Shaders::new(&base);
    let enhancements = enhancements::Shaders::new(&base);
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        {
            let stage = stage.clone();
            let since = since.clone();
            let frames = frames.clone();
            move || {
                let index = stage.get();
                if since.get().elapsed().as_millis() >= 1500 && frames.borrow()[index].is_some() {
                    if index == 10 {
                        engine.stop().unwrap();
                        weak.upgrade().unwrap().hide().unwrap();
                        slint::quit_event_loop().unwrap();
                        return;
                    }
                    let (settings, anime, _) = case(index + 1);
                    enhancements
                        .apply(engine.mpv(), &shaders, &anime, &settings)
                        .unwrap();
                    stage.set(index + 1);
                    since.set(Instant::now());
                }
                weak.upgrade().unwrap().window().request_redraw();
            }
        },
    );
    app.run().unwrap();
    let frames = frames.borrow();
    let reference = frames[0].unwrap();
    for (rgb, shade) in reference.iter().zip(shades) {
        assert!(
            rgb.iter().all(|v| (*v - f64::from(shade)).abs() < 2.),
            "fixture did not render: {reference:?}"
        );
    }
    for (index, frame) in frames.iter().enumerate() {
        let frame = frame.expect("missing colour sample");
        for (patch, rgb) in frame.iter().enumerate() {
            for channel in 0..3 {
                assert!((rgb[channel] - reference[patch][channel]).abs() < 4., "colour corruption at stage {index}, patch {patch}: {rgb:?}, expected {:?}; captures in {}", reference[patch], base.display());
            }
        }
    }
    std::fs::remove_dir_all(base).unwrap();
}
