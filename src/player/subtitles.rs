use super::*;
use crate::subtitles::{self as provider, Candidate};

#[derive(Default)]
pub(super) struct Session {
    pub epoch: u64,
    request: u64,
    pub ready: bool,
    pub original_url: Option<String>,
    candidates: Vec<Candidate>,
    job: Option<tokio::task::AbortHandle>,
    downloading: bool,
}

impl Session {
    fn cancel(&mut self) {
        self.request = self.request.wrapping_add(1);
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.downloading = false;
    }

    pub fn reset(&mut self) {
        self.cancel();
        self.epoch = self.epoch.wrapping_add(1);
        self.ready = false;
        self.original_url = None;
        self.candidates.clear();
    }

    fn matches(&self, epoch: u64, request: u64) -> bool {
        self.ready && self.epoch == epoch && self.request == request
    }
}

pub(super) fn reset_ui(app: &AppWindow) {
    app.set_player_subtitle_search_results(ModelRc::default());
    app.set_player_subtitle_search_busy(false);
    app.set_player_subtitle_search_available(false);
    app.set_player_subtitle_search_error(false);
    app.set_player_subtitle_search_status("".into());
    app.set_player_subtitle_download_id("".into());
}

pub(super) fn ended(state: &Arc<Mutex<PlayerPlaybackState>>, weak: &slint::Weak<AppWindow>) {
    let epoch = {
        let mut state = state.lock().unwrap();
        // A loadfile replacement ends the old file while the new file is opening.
        if !state.subtitles.ready {
            return;
        }
        state.active = false;
        state.subtitles.reset();
        state.segments.reset();
        state.subtitles.epoch
    };
    let state = state.clone();
    let _ = weak.upgrade_in_event_loop(move |app| {
        if state.lock().unwrap().subtitles.epoch == epoch {
            reset_ui(&app);
            super::segments::reset_ui(&app);
        }
    });
}

pub(super) fn cancel_download(
    state: &Arc<Mutex<PlayerPlaybackState>>,
    weak: &slint::Weak<AppWindow>,
) {
    let mut state = state.lock().unwrap();
    if state.subtitles.downloading {
        state.subtitles.cancel();
        if let Some(app) = weak.upgrade() {
            app.set_player_subtitle_download_id("".into());
            app.set_player_subtitle_search_status("".into());
        }
    }
}

pub(super) fn register(
    app: &AppWindow,
    engine: Arc<PlayerEngine>,
    state: Arc<Mutex<PlayerPlaybackState>>,
    client: PutioClient,
    config: Arc<ConfigStore>,
    rt: Arc<Runtime>,
) {
    let weak = app.as_weak();
    app.on_player_search_subtitles({
        let state = state.clone();
        let client = client.clone();
        let config = config.clone();
        let rt = rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut playback = state.lock().unwrap();
            if !playback.active || !playback.subtitles.ready {
                return;
            }
            let Some(file_id) = playback
                .current_index
                .and_then(|i| playback.queue.get(i))
                .map(|i| i.file_id)
            else {
                return;
            };
            let session = &mut playback.subtitles;
            session.cancel();
            session.candidates.clear();
            let (epoch, request, original_url) =
                (session.epoch, session.request, session.original_url.clone());
            app.set_player_subtitle_search_results(ModelRc::default());
            app.set_player_subtitle_download_id("".into());
            app.set_player_subtitle_search_busy(true);
            app.set_player_subtitle_search_error(false);
            app.set_player_subtitle_search_status("Searching put.io…".into());
            let weak = weak.clone();
            let state = state.clone();
            let client = client.clone();
            let token = config.oauth_token();
            let job = rt.spawn(async move {
                let progress = |message: &str| {
                    let message = message.to_string();
                    let state = state.clone();
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        if state.lock().unwrap().subtitles.matches(epoch, request) {
                            app.set_player_subtitle_search_status(message.into());
                        }
                    });
                };
                let result =
                    provider::search(&client, &token, file_id, original_url, progress).await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    let mut playback = state.lock().unwrap();
                    let session = &mut playback.subtitles;
                    if !session.matches(epoch, request) {
                        return;
                    }
                    session.job = None;
                    app.set_player_subtitle_search_busy(false);
                    match result {
                        Ok(result) => {
                            let rows: Vec<_> = result
                                .candidates
                                .iter()
                                .map(|c| PlayerTrack {
                                    id: c.id.as_str().into(),
                                    name: c.name.as_str().into(),
                                    detail: format!("{} · {}", c.language, c.source).into(),
                                })
                                .collect();
                            session.candidates = result.candidates;
                            app.set_player_subtitle_search_results(ModelRc::from(Rc::new(
                                VecModel::from(rows),
                            )));
                            app.set_player_subtitle_search_status(result.note.into());
                        }
                        Err(error) => {
                            app.set_player_subtitle_search_error(true);
                            app.set_player_subtitle_search_status(error.to_string().into());
                        }
                    }
                });
            });
            session.job = Some(job.abort_handle());
        }
    });

    let weak = app.as_weak();
    app.on_player_select_subtitle_result(move |id| {
        let Some(app) = weak.upgrade() else { return; };
        let mut playback = state.lock().unwrap();
        if !playback.active || !playback.subtitles.ready { return; }
        let Some(file_id) = playback.current_index.and_then(|i| playback.queue.get(i)).map(|i| i.file_id) else { return; };
        let session = &mut playback.subtitles;
        let Some(candidate) = session.candidates.iter().find(|c| c.id == id.as_str()).cloned() else { return; };
        session.cancel();
        session.downloading = true;
        let (epoch, request) = (session.epoch, session.request);
        app.set_player_subtitle_download_id(id);
        app.set_player_subtitle_search_error(false);
        app.set_player_subtitle_search_status("Downloading subtitle…".into());
        let weak = weak.clone();
        let state = state.clone();
        let client = client.clone();
        let engine = engine.clone();
        let token = config.oauth_token();
        let job = rt.spawn(async move {
            let result = provider::download(&client, &token, file_id, &candidate).await;
            let _ = weak.upgrade_in_event_loop(move |app| {
                let mut playback = state.lock().unwrap();
                let session = &mut playback.subtitles;
                if !session.matches(epoch, request) { return; }
                session.job = None;
                session.downloading = false;
                app.set_player_subtitle_download_id("".into());
                let loaded = result.and_then(|path| engine.add_subtitle(&path, &candidate.name, &candidate.language_code));
                match loaded {
                    Ok(()) => {
                        let _ = engine.set_sub_visibility(app.get_player_subtitle_render_mode() == 0);
                        let tracks = read_tracks(engine.mpv());
                        playback.last_subtitle_track = Some(tracks.selected_subtitle.clone());
                        apply_tracks(&app, tracks);
                        app.set_player_subtitle_search_status("Subtitle loaded.".into());
                    }
                    Err(_) => {
                        app.set_player_subtitle_search_error(true);
                        app.set_player_subtitle_search_status("Couldn't load this subtitle. Select it to retry, or try another result.".into());
                    }
                }
            });
        });
        session.job = Some(job.abort_handle());
    });
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn rejects_results_from_previous_files_and_requests() {
        let mut session = Session {
            ready: true,
            ..Default::default()
        };
        assert!(session.matches(0, 0));
        session.cancel();
        assert!(!session.matches(0, 0));
        assert!(session.matches(0, 1));
        session.reset();
        assert!(!session.matches(0, 1));
        assert!(!session.matches(1, 2));
        session.ready = true;
        assert!(session.matches(1, 2));
    }
}
