//! Opt-in desktop check using the application's real Slint/libmpv render path.
//! Generate a short looping sample, then run with PUTMPV_ANIME4K_SAMPLE set:
//! cargo test --profile dev-fast anime4k_gpu_smoke -- --ignored --nocapture
use super::*;
use crate::player::{anime4k, mpv_nodes};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Read the actual video FBO, since Slint's screenshots omit borrowed GL images.
/// Grey patches expose colour casts even when every CNN pass compiles and runs.
#[test]
#[ignore = "requires an OpenGL desktop"]
fn anime4k_colour_smoke() {
    use glow::HasContext;

    match std::env::var("PUTMPV_TEST_GL").as_deref() {
        Ok("es") => slint::BackendSelector::new()
            .require_opengl_es()
            .select()
            .unwrap(),
        _ => crate::player::select_graphics_backend().unwrap(),
    }
    let base = std::env::temp_dir().join(format!("putmpv-anime4k-colour-{}", std::process::id()));
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
    let app = AppWindow::new().unwrap();
    let engine = Arc::new(PlayerEngine::new().unwrap());
    engine
        .mpv()
        .set_property("image-display-duration", "inf")
        .unwrap();
    engine.mpv().set_property("pause", true).unwrap();
    app.set_view(crate::app_ui::VIEW_PLAYER);
    let stage = Rc::new(Cell::new(0usize));
    let since = Rc::new(Cell::new(Instant::now()));
    let frames = Rc::new(RefCell::new(vec![None; 14]));
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
                    if index == 13 {
                        engine.stop().unwrap();
                        weak.upgrade().unwrap().hide().unwrap();
                        slint::quit_event_loop().unwrap();
                        return;
                    }
                    let settings = if index < 12 {
                        anime4k::Settings {
                            enabled: true,
                            mode: anime4k::Mode::ALL[index % 6],
                            quality: if index < 6 {
                                anime4k::Quality::Fast
                            } else {
                                anime4k::Quality::Hq
                            },
                        }
                    } else {
                        anime4k::Settings::default()
                    };
                    shaders.apply(engine.mpv(), &settings).unwrap();
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

#[test]
#[ignore = "requires an OpenGL desktop and PUTMPV_ANIME4K_SAMPLE video"]
fn anime4k_gpu_smoke() {
    let _ = tracing_subscriber::fmt().try_init();
    let sample = std::env::var("PUTMPV_ANIME4K_SAMPLE").expect("set a local test video path");
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
    let observations = Arc::new(Mutex::new(vec![Vec::<(usize, f64)>::new(); 14]));
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
                    if anime4k::shader_error(prefix, level, text) {
                        errors.lock().unwrap().push(text.to_owned());
                    }
                    if text.starts_with("pass '") {
                        passes += 1;
                        active += usize::from(text.contains("Anime4K"));
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
    let base = std::env::temp_dir().join(format!("putmpv-anime4k-gpu-{}", std::process::id()));
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
                if index >= 13 {
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
                let next = if index < 12 {
                    anime4k::Settings {
                        enabled: true,
                        mode: anime4k::Mode::ALL[index % 6],
                        quality: if index < 6 {
                            anime4k::Quality::Fast
                        } else {
                            anime4k::Quality::Hq
                        },
                    }
                } else {
                    anime4k::Settings::default()
                };
                shaders.apply(engine.mpv(), &next).unwrap();
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
            "Anime4K stage {index}: {active} active passes; median GPU frame {:.2} ms",
            timings[timings.len() / 2]
        );
        if (1..13).contains(&index) {
            assert!(active > 3, "CNN passes did not run at stage {index}");
        } else {
            assert_eq!(active, 0);
        }
    }
    std::fs::remove_dir_all(base).unwrap();
}

/// Exercise the actual GLES context and the settings controller's guard, without
/// loading remote media or changing the user's configuration.
#[test]
#[ignore = "requires a GLES desktop"]
fn anime4k_gles_guard_smoke() {
    slint::BackendSelector::new()
        .require_opengl_es()
        .select()
        .unwrap();
    let app = AppWindow::new().unwrap();
    let engine = Arc::new(PlayerEngine::new().unwrap());
    let base = std::env::temp_dir().join(format!("putmpv-gles-guard-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let config = Arc::new(crate::storage::config::ConfigStore::for_test(
        base.join("config.json"),
    ));
    let custom = vec!["/custom/preserve.glsl".to_owned()];
    mpv_nodes::set_strings(engine.mpv(), "glsl-shaders", &custom).unwrap();
    let controller =
        crate::player::settings_ui::Controller::install(&app, Some(engine.clone()), config);
    app.invoke_player_setting_flag("anime4k-enabled".into(), true);
    controller.flush().unwrap();
    register_renderer(&app, engine.clone(), crate::app_ui::VIEW_PLAYER);
    let weak = app.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_secs(2), move || {
        let app = weak.upgrade().unwrap();
        if engine.anime4k_blocked() {
            let values = app.get_player_settings_values();
            assert!(values.anime4k_enabled);
            assert!(values.anime4k_warning);
            assert_eq!(values.anime4k_status, anime4k::GLES_WARNING);
            assert_eq!(
                mpv_nodes::strings(engine.mpv(), "glsl-shaders").unwrap(),
                custom
            );
            app.invoke_player_setting_flag("anime4k-enabled".into(), true);
            assert!(app.invoke_player_settings_flush());
            assert!(app.get_player_settings_values().anime4k_enabled);
            eprintln!("NVIDIA GLES guard blocked Anime4K and preserved custom shaders");
        } else {
            assert!(app.get_player_settings_values().anime4k_enabled);
            eprintln!("Non-NVIDIA GLES retained Anime4K");
        }
        app.hide().unwrap();
        slint::quit_event_loop().unwrap();
    });
    app.run().unwrap();
    std::fs::remove_dir_all(base).unwrap();
}
