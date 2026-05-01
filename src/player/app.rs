use std::sync::{Arc, Mutex};

use slint::ComponentHandle;
use tokio::runtime::Runtime;
use tracing::warn;

use crate::putio::{self, PutioClient};
use crate::storage::config::ConfigStore;
use crate::AppWindow;

use super::{PlayerEngine, PlayerRenderer};

#[derive(Default)]
struct PlayerPlaybackState {
    active: bool,
    tried_fallback: bool,
    fallback_url: Option<String>,
}

#[derive(Clone)]
pub struct EmbeddedPlayer {
    engine: Option<Arc<PlayerEngine>>,
    playback_state: Arc<Mutex<PlayerPlaybackState>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    player_view: i32,
    files_view: i32,
}

impl EmbeddedPlayer {
    pub fn install(
        app: &AppWindow,
        client: PutioClient,
        config: Arc<ConfigStore>,
        rt: Arc<Runtime>,
        player_view: i32,
        files_view: i32,
    ) -> Self {
        let engine = match PlayerEngine::new() {
            Ok(engine) => Some(Arc::new(engine)),
            Err(e) => {
                warn!("embedded mpv player unavailable: {e}");
                None
            }
        };
        let playback_state = Arc::new(Mutex::new(PlayerPlaybackState::default()));

        if let Some(engine) = engine.clone() {
            register_renderer(app, engine, player_view);
        }
        if let Some(engine) = engine.clone() {
            register_events(app, engine, playback_state.clone());
        }

        let player = Self {
            engine,
            playback_state,
            client,
            config,
            rt,
            player_view,
            files_view,
        };
        player.register_close(app);
        player
    }

    pub fn play(&self, app: &AppWindow, file_id: u64, title: String) {
        let Some(engine) = self.engine.clone() else {
            app.set_player_title("Embedded mpv player is unavailable.".into());
            app.set_view(self.player_view);
            return;
        };

        let token = self.config.oauth_token();
        if token.is_empty() {
            app.set_player_title("Sign in before playing media.".into());
            app.set_view(self.player_view);
            return;
        }

        let fallback_url = putio::stream::fallback_mp4_stream_url(&token, file_id);
        {
            let mut state = self.playback_state.lock().unwrap();
            state.active = true;
            state.tried_fallback = false;
            state.fallback_url = Some(fallback_url.clone());
        }

        app.set_player_title(format!("Opening {title}...").into());
        app.set_view(self.player_view);

        let weak = app.as_weak();
        let client = self.client.clone();
        let playback_title = title.clone();
        self.rt.spawn(async move {
            let message = match putio::stream::resolve_play_url(&client, &token, file_id).await {
                Ok(url) => match engine.load(&url) {
                    Ok(()) => playback_title,
                    Err(e) => format!("Could not start embedded playback: {e}"),
                },
                Err(e) => match engine.load(&fallback_url) {
                    Ok(()) => playback_title,
                    Err(load_err) => {
                        format!("Could not resolve original stream: {e}; fallback failed: {load_err}")
                    }
                },
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_player_title(message.into());
                app.window().request_redraw();
            });
        });
    }

    fn register_close(&self, app: &AppWindow) {
        let weak = app.as_weak();
        let engine = self.engine.clone();
        let playback_state = self.playback_state.clone();
        let files_view = self.files_view;
        app.on_player_close(move || {
            if let Some(engine) = engine.as_ref() {
                if let Err(e) = engine.stop() {
                    warn!("could not stop embedded mpv playback: {e}");
                }
            }
            {
                let mut state = playback_state.lock().unwrap();
                state.active = false;
                state.tried_fallback = false;
                state.fallback_url = None;
            }
            if let Some(app) = weak.upgrade() {
                app.set_view(files_view);
            }
        });
    }
}

fn register_renderer(app: &AppWindow, engine: Arc<PlayerEngine>, player_view: i32) {
    let weak = app.as_weak();
    let mut renderer: Option<PlayerRenderer> = None;
    let notifier = app
        .window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                    warn!("embedded player needs Slint's OpenGL renderer");
                    return;
                };

                let gl = unsafe {
                    glow::Context::from_loader_function_cstr(|name| get_proc_address(name))
                };
                match PlayerRenderer::new(engine.mpv(), gl, get_proc_address) {
                    Ok(mut player_renderer) => {
                        let weak = weak.clone();
                        player_renderer.set_update_callback(move || {
                            let _ = weak.upgrade_in_event_loop(|app| app.window().request_redraw());
                        });
                        renderer = Some(player_renderer);
                    }
                    Err(e) => warn!("could not create embedded mpv renderer: {e}"),
                }
            }
            slint::RenderingState::BeforeRendering => {
                let (Some(renderer), Some(app)) = (renderer.as_mut(), weak.upgrade()) else {
                    return;
                };
                if app.get_view() != player_view {
                    return;
                }
                let width = app.get_player_texture_width().max(1.0) as u32;
                let height = app.get_player_texture_height().max(1.0) as u32;
                match renderer.render(width, height) {
                    Ok(Some(texture)) => app.set_player_texture(texture),
                    Ok(None) => {}
                    Err(e) => warn!("embedded mpv render failed: {e}"),
                }
            }
            slint::RenderingState::RenderingTeardown => {
                renderer = None;
            }
            _ => {}
        });
    if let Err(e) = notifier {
        warn!("embedded player rendering notifier unavailable: {e}");
    }
}

fn register_events(
    app: &AppWindow,
    engine: Arc<PlayerEngine>,
    playback_state: Arc<Mutex<PlayerPlaybackState>>,
) {
    match engine.create_event_client() {
        Ok(mut event_client) => {
            let weak = app.as_weak();
            std::thread::spawn(move || loop {
                match event_client.wait_event(1.0) {
                    Some(Ok(libmpv2::events::Event::EndFile(_))) => {
                        playback_state.lock().unwrap().active = false;
                    }
                    Some(Ok(_)) | None => {}
                    Some(Err(e)) => {
                        let error_message = e.to_string();
                        let fallback_url = {
                            let mut state = playback_state.lock().unwrap();
                            if !state.active || state.tried_fallback {
                                None
                            } else {
                                state.tried_fallback = true;
                                state.fallback_url.clone()
                            }
                        };

                        if let Some(fallback_url) = fallback_url {
                            warn!(
                                "embedded mpv original playback failed, trying fallback: {error_message}"
                            );
                            if let Err(load_err) = engine.load(&fallback_url) {
                                let load_err = load_err.to_string();
                                let _ = weak.upgrade_in_event_loop(move |app| {
                                    app.set_player_title(
                                        format!("Embedded playback failed: {load_err}").into(),
                                    );
                                });
                            }
                        } else {
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_player_title(
                                    format!("Embedded playback failed: {error_message}").into(),
                                );
                            });
                        }
                    }
                }

                if weak.upgrade_in_event_loop(|_| {}).is_err() {
                    break;
                }
            });
        }
        Err(e) => warn!("could not create embedded mpv event client: {e}"),
    }
}
