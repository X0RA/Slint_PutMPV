use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use libmpv2::{
    events::{Event, PropertyData},
    mpv_end_file_reason, Mpv,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio::runtime::Runtime;
use tracing::warn;

use crate::putio::{self, PutioClient};
use crate::storage::config::ConfigStore;
use crate::sync::watch_session::WatchSyncService;
use crate::{AppWindow, PlayerPlaylistItem, PlayerTrack};

use super::{PlayerEngine, PlayerRenderer};

#[derive(Default)]
struct PlayerPlaybackState {
    active: bool,
    tried_fallback: bool,
    fallback_url: Option<String>,
    last_subtitle_track: Option<SharedString>,
    queue: Vec<PlaybackQueueItem>,
    current_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct PlaybackQueueItem {
    pub file_id: u64,
    pub title: String,
    pub meta: String,
}

#[derive(Clone)]
pub struct EmbeddedPlayer {
    engine: Option<Arc<PlayerEngine>>,
    playback_state: Arc<Mutex<PlayerPlaybackState>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    watch_sync: Arc<WatchSyncService>,
    player_view: i32,
    files_view: i32,
}

impl EmbeddedPlayer {
    pub fn install(
        app: &AppWindow,
        client: PutioClient,
        config: Arc<ConfigStore>,
        rt: Arc<Runtime>,
        watch_sync: Arc<WatchSyncService>,
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
            register_events(
                app,
                engine,
                playback_state.clone(),
                client.clone(),
                config.clone(),
                rt.clone(),
                watch_sync.clone(),
                player_view,
            );
        }
        if let Some(engine) = engine.clone() {
            register_callbacks(
                app,
                engine,
                playback_state.clone(),
                client.clone(),
                config.clone(),
                rt.clone(),
                watch_sync.clone(),
                player_view,
            );
        }

        let player = Self {
            engine,
            playback_state,
            client,
            config,
            rt,
            watch_sync,
            player_view,
            files_view,
        };
        player.register_close(app);
        player
    }

    pub fn play_queue(&self, app: &AppWindow, queue: Vec<PlaybackQueueItem>, file_id: u64) {
        let Some(engine) = self.engine.clone() else {
            app.set_player_title("Embedded mpv player is unavailable.".into());
            app.set_view(self.player_view);
            return;
        };

        if queue.is_empty() {
            return;
        }

        let index = queue
            .iter()
            .position(|item| item.file_id == file_id)
            .unwrap_or(0);
        set_playlist_model(app, &queue, index);

        {
            let mut state = self.playback_state.lock().unwrap();
            state.queue = queue.clone();
            state.current_index = Some(index);
        }

        start_queue_item(
            app,
            engine,
            self.playback_state.clone(),
            self.client.clone(),
            self.config.clone(),
            self.rt.clone(),
            self.watch_sync.clone(),
            self.player_view,
            queue[index].clone(),
            index,
        );
    }

    fn register_close(&self, app: &AppWindow) {
        let weak = app.as_weak();
        let engine = self.engine.clone();
        let playback_state = self.playback_state.clone();
        let watch_sync = self.watch_sync.clone();
        let files_view = self.files_view;
        app.on_player_close(move || {
            watch_sync.on_session_end();
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
                state.queue.clear();
                state.current_index = None;
            }
            if let Some(app) = weak.upgrade() {
                app.set_player_playlist_current_id("".into());
                set_playlist_nav_state(&app, 0, None);
                app.set_player_playlist_items(ModelRc::from(Rc::new(VecModel::from(Vec::<
                    PlayerPlaylistItem,
                >::new(
                )))));
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
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    watch_sync: Arc<WatchSyncService>,
    player_view: i32,
) {
    match engine.create_event_client() {
        Ok(mut event_client) => {
            if let Err(e) = PlayerEngine::observe_properties(&event_client) {
                warn!("could not observe embedded player properties: {e}");
            }
            let weak = app.as_weak();
            std::thread::spawn(move || loop {
                match event_client.wait_event(1.0) {
                    Some(Ok(Event::EndFile(reason))) => {
                        if reason == mpv_end_file_reason::Eof {
                            watch_sync.on_eof();
                            let next = {
                                let state = playback_state.lock().unwrap();
                                let next_index = state.current_index.map(|idx| idx + 1);
                                next_index.and_then(|idx| {
                                    state.queue.get(idx).cloned().map(|item| (idx, item))
                                })
                            };
                            if let Some((idx, item)) = next {
                                let _ = weak.upgrade_in_event_loop({
                                    let engine = engine.clone();
                                    let playback_state = playback_state.clone();
                                    let client = client.clone();
                                    let config = config.clone();
                                    let rt = rt.clone();
                                    let watch_sync = watch_sync.clone();
                                    move |app| {
                                        start_queue_item(
                                            &app,
                                            engine,
                                            playback_state,
                                            client,
                                            config,
                                            rt,
                                            watch_sync,
                                            player_view,
                                            item,
                                            idx,
                                        );
                                    }
                                });
                            } else {
                                playback_state.lock().unwrap().active = false;
                            }
                        } else {
                            watch_sync.on_session_end();
                            playback_state.lock().unwrap().active = false;
                        }
                    }
                    Some(Ok(Event::FileLoaded)) => {
                        let duration = get_f64(&event_client, "duration").unwrap_or(0.0);
                        let resume = {
                            let state = playback_state.lock().unwrap();
                            state
                                .current_index
                                .and_then(|idx| state.queue.get(idx))
                                .and_then(|item| watch_sync.start_session(item.file_id, duration))
                        };
                        if let Some(position) = resume {
                            if let Err(e) = engine.seek(position) {
                                warn!("could not auto-resume playback: {e}");
                            }
                        }
                        let tracks = read_tracks(&event_client);
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            apply_tracks(&app, tracks);
                        });
                    }
                    Some(Ok(Event::AudioReconfig)) => {
                        let tracks = read_tracks(&event_client);
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            apply_tracks(&app, tracks);
                        });
                    }
                    Some(Ok(Event::Seek)) => {
                        watch_sync.on_seek();
                    }
                    Some(Ok(Event::PropertyChange { name, change, .. })) => {
                        if name == "track-list/count" {
                            let tracks = read_tracks(&event_client);
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                apply_tracks(&app, tracks);
                            });
                        } else {
                            apply_property_change(&weak, &watch_sync, name, change);
                        }
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

fn register_callbacks(
    app: &AppWindow,
    engine: Arc<PlayerEngine>,
    playback_state: Arc<Mutex<PlayerPlaybackState>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    watch_sync: Arc<WatchSyncService>,
    player_view: i32,
) {
    let weak = app.as_weak();
    let engine_for_play = engine.clone();
    app.on_player_toggle_play(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let paused = !app.get_player_paused();
        app.set_player_paused(paused);
        if let Err(e) = engine_for_play.set_pause(paused) {
            warn!("could not toggle embedded mpv playback: {e}");
        }
    });

    let engine_for_seek = engine.clone();
    app.on_player_seek(move |seconds| {
        if let Err(e) = engine_for_seek.seek(seconds.into()) {
            warn!("could not seek embedded mpv playback: {e}");
        }
    });

    let weak = app.as_weak();
    let engine_for_volume = engine.clone();
    app.on_player_set_volume(move |volume| {
        if let Some(app) = weak.upgrade() {
            app.set_player_volume(volume);
            if volume > 0.0 && app.get_player_muted() {
                app.set_player_muted(false);
                let _ = engine_for_volume.set_mute(false);
            }
        }
        if let Err(e) = engine_for_volume.set_volume(volume.into()) {
            warn!("could not set embedded mpv volume: {e}");
        }
    });

    let weak = app.as_weak();
    let engine_for_mute = engine.clone();
    app.on_player_toggle_mute(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let muted = !app.get_player_muted();
        app.set_player_muted(muted);
        if let Err(e) = engine_for_mute.set_mute(muted) {
            warn!("could not toggle embedded mpv mute: {e}");
        }
    });

    let weak = app.as_weak();
    let engine_for_subtitle = engine.clone();
    let subtitle_state = playback_state.clone();
    app.on_player_select_subtitle(move |track_id| {
        if !track_id.is_empty() {
            subtitle_state.lock().unwrap().last_subtitle_track = Some(track_id.clone());
        }
        if let Err(e) = engine_for_subtitle.set_sid(track_id.as_str()) {
            warn!("could not select embedded mpv subtitle track: {e}");
        }
        if let Some(app) = weak.upgrade() {
            if let Err(e) =
                engine_for_subtitle.set_sub_visibility(app.get_player_subtitle_render_mode() == 0)
            {
                warn!("could not update embedded mpv subtitle visibility: {e}");
            }
        }
    });

    let weak = app.as_weak();
    let engine_for_captions = engine.clone();
    let captions_state = playback_state.clone();
    app.on_player_toggle_captions(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };

        let current = app.get_player_selected_subtitle();
        let next = if current.is_empty() {
            captions_state
                .lock()
                .unwrap()
                .last_subtitle_track
                .clone()
                .or_else(|| first_available_subtitle(&app))
                .unwrap_or_default()
        } else {
            captions_state.lock().unwrap().last_subtitle_track = Some(current);
            SharedString::from("")
        };

        app.set_player_selected_subtitle(next.clone());
        if let Err(e) = engine_for_captions.set_sid(next.as_str()) {
            warn!("could not toggle embedded mpv captions: {e}");
        }
        if let Err(e) =
            engine_for_captions.set_sub_visibility(app.get_player_subtitle_render_mode() == 0)
        {
            warn!("could not update embedded mpv subtitle visibility: {e}");
        }
    });

    let weak = app.as_weak();
    let engine_for_subtitle_mode = engine.clone();
    app.on_player_subtitle_render_mode_changed(move |mode| {
        let mode = mode.clamp(0, 1);
        if let Some(app) = weak.upgrade() {
            app.set_player_subtitle_render_mode(mode);
        }
        if let Err(e) = engine_for_subtitle_mode.set_sub_visibility(mode == 0) {
            warn!("could not switch embedded mpv subtitle rendering mode: {e}");
        }
    });

    let engine_for_audio = engine.clone();
    app.on_player_select_audio(move |track_id| {
        if let Err(e) = engine_for_audio.set_aid(track_id.as_str()) {
            warn!("could not select embedded mpv audio track: {e}");
        }
    });

    let weak = app.as_weak();
    let engine_for_playlist = engine.clone();
    let playlist_state = playback_state.clone();
    let play_client = client.clone();
    let play_config = config.clone();
    let play_rt = rt.clone();
    let play_watch_sync = watch_sync.clone();
    app.on_player_playlist_play(move |file_id| {
        let Ok(file_id) = file_id.as_str().parse::<u64>() else {
            return;
        };
        let Some(app) = weak.upgrade() else {
            return;
        };
        let item = {
            let state = playlist_state.lock().unwrap();
            state
                .queue
                .iter()
                .enumerate()
                .find(|(_, item)| item.file_id == file_id)
                .map(|(idx, item)| (idx, item.clone()))
        };
        if let Some((idx, item)) = item {
            start_queue_item(
                &app,
                engine_for_playlist.clone(),
                playlist_state.clone(),
                play_client.clone(),
                play_config.clone(),
                play_rt.clone(),
                play_watch_sync.clone(),
                player_view,
                item,
                idx,
            );
        }
    });

    let weak = app.as_weak();
    let engine_for_playlist_previous = engine.clone();
    let previous_state = playback_state.clone();
    let previous_client = client.clone();
    let previous_config = config.clone();
    let previous_rt = rt.clone();
    let previous_watch_sync = watch_sync.clone();
    app.on_player_playlist_previous(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let item = adjacent_queue_item(&previous_state, -1);
        if let Some((idx, item)) = item {
            start_queue_item(
                &app,
                engine_for_playlist_previous.clone(),
                previous_state.clone(),
                previous_client.clone(),
                previous_config.clone(),
                previous_rt.clone(),
                previous_watch_sync.clone(),
                player_view,
                item,
                idx,
            );
        }
    });

    let weak = app.as_weak();
    let engine_for_playlist_next = engine.clone();
    let next_state = playback_state.clone();
    app.on_player_playlist_next(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let item = adjacent_queue_item(&next_state, 1);
        if let Some((idx, item)) = item {
            start_queue_item(
                &app,
                engine_for_playlist_next.clone(),
                next_state.clone(),
                client.clone(),
                config.clone(),
                rt.clone(),
                watch_sync.clone(),
                player_view,
                item,
                idx,
            );
        }
    });

    let weak = app.as_weak();
    app.on_player_toggle_fullscreen(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let fullscreen = !app.get_player_fullscreen();
        app.set_player_fullscreen(fullscreen);
        app.window().set_fullscreen(fullscreen);
    });

    let weak = app.as_weak();
    app.on_player_exit_fullscreen(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_player_fullscreen() {
            app.set_player_fullscreen(false);
            app.window().set_fullscreen(false);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn start_queue_item(
    app: &AppWindow,
    engine: Arc<PlayerEngine>,
    playback_state: Arc<Mutex<PlayerPlaybackState>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
    watch_sync: Arc<WatchSyncService>,
    player_view: i32,
    item: PlaybackQueueItem,
    index: usize,
) {
    let token = config.oauth_token();
    if token.is_empty() {
        app.set_player_title("Sign in before playing media.".into());
        app.set_view(player_view);
        return;
    }

    let fallback_url = putio::stream::fallback_mp4_stream_url(&token, item.file_id);
    watch_sync.on_session_end();
    {
        let mut state = playback_state.lock().unwrap();
        state.active = true;
        state.tried_fallback = false;
        state.fallback_url = Some(fallback_url.clone());
        state.last_subtitle_track = None;
        state.current_index = Some(index);
    }

    app.set_player_playlist_current_id(item.file_id.to_string().into());
    let queue_len = playback_state.lock().unwrap().queue.len();
    set_playlist_nav_state(app, queue_len, Some(index));
    app.set_player_title(format!("Opening {}...", item.title).into());
    reset_player_state(app);
    if let Err(e) = engine.set_sub_visibility(true) {
        warn!("could not reset embedded mpv subtitle visibility: {e}");
    }
    app.set_view(player_view);

    let weak = app.as_weak();
    let playback_title = item.title.clone();
    let file_id = item.file_id;
    rt.spawn(async move {
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

fn set_playlist_model(app: &AppWindow, queue: &[PlaybackQueueItem], current_index: usize) {
    let rows = queue
        .iter()
        .map(|item| PlayerPlaylistItem {
            file_id: item.file_id.to_string().into(),
            title: item.title.as_str().into(),
            meta: item.meta.as_str().into(),
        })
        .collect::<Vec<_>>();
    app.set_player_playlist_items(ModelRc::from(Rc::new(VecModel::from(rows))));
    if let Some(current) = queue.get(current_index) {
        app.set_player_playlist_current_id(current.file_id.to_string().into());
    }
    set_playlist_nav_state(app, queue.len(), Some(current_index));
}

fn set_playlist_nav_state(app: &AppWindow, queue_len: usize, current_index: Option<usize>) {
    let has_previous = current_index.is_some_and(|idx| idx > 0 && idx < queue_len);
    let has_next = current_index.is_some_and(|idx| idx + 1 < queue_len);
    app.set_player_playlist_has_previous(has_previous);
    app.set_player_playlist_has_next(has_next);
}

fn adjacent_queue_item(
    playback_state: &Arc<Mutex<PlayerPlaybackState>>,
    offset: isize,
) -> Option<(usize, PlaybackQueueItem)> {
    let state = playback_state.lock().unwrap();
    let current = state.current_index?;
    let next = current.checked_add_signed(offset)?;
    state.queue.get(next).cloned().map(|item| (next, item))
}

fn reset_player_state(app: &AppWindow) {
    app.set_player_paused(false);
    app.set_player_position(0.0);
    app.set_player_buffered_fraction(0.0);
    app.set_player_position_label("0:00".into());
    app.set_player_duration(0.0);
    app.set_player_duration_label("0:00".into());
    app.set_player_subtitle_text("".into());
    app.set_player_subtitle_render_mode(0);
    app.set_player_selected_subtitle("".into());
    app.set_player_selected_audio("".into());
    app.set_player_subtitle_tracks(ModelRc::from(Rc::new(VecModel::from(
        Vec::<PlayerTrack>::new(),
    ))));
    app.set_player_audio_tracks(ModelRc::from(Rc::new(VecModel::from(
        Vec::<PlayerTrack>::new(),
    ))));
}

fn first_available_subtitle(app: &AppWindow) -> Option<SharedString> {
    let tracks = app.get_player_subtitle_tracks();
    for index in 0..tracks.row_count() {
        let Some(track) = tracks.row_data(index) else {
            continue;
        };
        if !track.id.is_empty() {
            return Some(track.id);
        }
    }
    None
}

#[derive(Debug)]
struct PlayerTracks {
    subtitles: Vec<PlayerTrack>,
    selected_subtitle: SharedString,
    audio: Vec<PlayerTrack>,
    selected_audio: SharedString,
}

fn apply_property_change(
    weak: &slint::Weak<AppWindow>,
    watch_sync: &WatchSyncService,
    name: &str,
    change: PropertyData<'_>,
) {
    match (name, change) {
        ("pause", PropertyData::Flag(paused)) => {
            watch_sync.on_pause(paused);
            let _ = weak.upgrade_in_event_loop(move |app| app.set_player_paused(paused));
        }
        ("time-pos", PropertyData::Double(position)) => {
            watch_sync.on_position(position, 0.0);
            let label = format_time(position);
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_player_position(position as f32);
                app.set_player_position_label(label.into());
            });
        }
        ("duration", PropertyData::Double(duration)) => {
            watch_sync.on_duration(duration);
            let label = format_time(duration);
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_player_duration(duration as f32);
                app.set_player_duration_label(label.into());
            });
        }
        ("demuxer-cache-time", PropertyData::Double(cache_time)) => {
            let _ = weak.upgrade_in_event_loop(move |app| {
                let duration = app.get_player_duration();
                let fraction = if duration > 0.0 {
                    (cache_time as f32 / duration).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                app.set_player_buffered_fraction(fraction);
            });
        }
        ("volume", PropertyData::Double(volume)) => {
            let _ = weak.upgrade_in_event_loop(move |app| app.set_player_volume(volume as f32));
        }
        ("mute", PropertyData::Flag(muted)) => {
            let _ = weak.upgrade_in_event_loop(move |app| app.set_player_muted(muted));
        }
        ("sub-text", PropertyData::Str(text)) | ("sub-text", PropertyData::OsdStr(text)) => {
            let text = text.to_owned();
            let _ =
                weak.upgrade_in_event_loop(move |app| app.set_player_subtitle_text(text.into()));
        }
        ("sid", PropertyData::Str(track_id)) | ("sid", PropertyData::OsdStr(track_id)) => {
            let track_id = normalize_track_id(track_id).to_owned();
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_player_selected_subtitle(track_id.into())
            });
        }
        ("aid", PropertyData::Str(track_id)) | ("aid", PropertyData::OsdStr(track_id)) => {
            let track_id = normalize_track_id(track_id).to_owned();
            let _ = weak
                .upgrade_in_event_loop(move |app| app.set_player_selected_audio(track_id.into()));
        }
        _ => {}
    }
}

fn apply_tracks(app: &AppWindow, tracks: PlayerTracks) {
    app.set_player_subtitle_tracks(ModelRc::from(Rc::new(VecModel::from(tracks.subtitles))));
    app.set_player_selected_subtitle(tracks.selected_subtitle);
    app.set_player_audio_tracks(ModelRc::from(Rc::new(VecModel::from(tracks.audio))));
    app.set_player_selected_audio(tracks.selected_audio);
}

fn read_tracks(mpv: &Mpv) -> PlayerTracks {
    let count = get_i64(mpv, "track-list/count").unwrap_or(0).max(0);
    let mut subtitles = vec![PlayerTrack {
        id: SharedString::from(""),
        name: SharedString::from("Off"),
        detail: SharedString::from(""),
    }];
    let mut audio = Vec::new();
    let mut selected_subtitle = SharedString::from("");
    let mut selected_audio = SharedString::from("");

    for index in 0..count {
        let prefix = format!("track-list/{index}");
        let Some(kind) = get_string(mpv, &format!("{prefix}/type")) else {
            continue;
        };
        let Some(id) = get_i64(mpv, &format!("{prefix}/id")) else {
            continue;
        };
        let id_string = id.to_string();
        let selected = get_bool(mpv, &format!("{prefix}/selected")).unwrap_or(false);
        let default = get_bool(mpv, &format!("{prefix}/default")).unwrap_or(false);
        let lang = get_string(mpv, &format!("{prefix}/lang"));
        let title = get_string(mpv, &format!("{prefix}/title"));
        let codec = get_string(mpv, &format!("{prefix}/codec"));

        let label_base = if kind == "sub" { "Subtitle" } else { "Audio" };
        let name = title
            .clone()
            .or(lang.clone())
            .unwrap_or_else(|| format!("{label_base} {id}"));
        let mut detail_parts = Vec::new();
        if let Some(lang) = lang {
            detail_parts.push(lang.to_uppercase());
        }
        if let Some(codec) = codec {
            detail_parts.push(codec.to_uppercase());
        }
        if default {
            detail_parts.push("Default".to_string());
        }
        let detail = detail_parts.join(" · ");

        let track = PlayerTrack {
            id: SharedString::from(id_string.as_str()),
            name: SharedString::from(name),
            detail: SharedString::from(detail),
        };

        match kind.as_str() {
            "sub" => {
                if selected {
                    selected_subtitle = SharedString::from(id_string.as_str());
                }
                subtitles.push(track);
            }
            "audio" => {
                if selected {
                    selected_audio = SharedString::from(id_string.as_str());
                }
                audio.push(track);
            }
            _ => {}
        }
    }

    PlayerTracks {
        subtitles,
        selected_subtitle,
        audio,
        selected_audio,
    }
}

fn get_string(mpv: &Mpv, name: &str) -> Option<String> {
    mpv.get_property::<String>(name)
        .ok()
        .filter(|s| !s.is_empty())
}

fn get_i64(mpv: &Mpv, name: &str) -> Option<i64> {
    mpv.get_property::<i64>(name).ok()
}

fn get_bool(mpv: &Mpv, name: &str) -> Option<bool> {
    mpv.get_property::<bool>(name).ok()
}

fn get_f64(mpv: &Mpv, name: &str) -> Option<f64> {
    mpv.get_property::<f64>(name).ok()
}

fn normalize_track_id(track_id: &str) -> &str {
    if track_id == "no" || track_id == "auto" {
        ""
    } else {
        track_id
    }
}

fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".to_string();
    }
    let seconds = seconds.round() as u64;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}
