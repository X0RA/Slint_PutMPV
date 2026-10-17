//! Playback-session boundary for skip markers. No context means no lookup, even on a cache hit.
use super::*;
use crate::segments::{self as markers, Kind, Segment, Service};

#[derive(Default)]
pub(super) struct Session {
    epoch: u64,
    duration: f64,
    position: f64,
    path: Option<String>,
    segments: Vec<Segment>,
    job: Option<tokio::task::AbortHandle>,
}
impl Session {
    pub fn reset(&mut self) {
        if let Some(job) = self.job.take() {
            job.abort();
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.duration = 0.0;
        self.position = 0.0;
        self.path = None;
        self.segments.clear();
    }
    fn active(&self) -> Option<&Segment> {
        self.segments.iter().find(|s| s.contains(self.position))
    }
    fn accepts(&self, epoch: u64) -> bool {
        self.epoch == epoch && self.path.is_some()
    }
}

pub(super) fn reset_ui(app: &AppWindow) {
    app.set_player_skip_label("".into());
    app.set_player_skip_next_available(false);
    app.set_player_skip_source("".into());
}

pub(super) fn load(
    mpv: &Mpv,
    weak: &slint::Weak<AppWindow>,
    state: &Arc<Mutex<PlayerPlaybackState>>,
    rt: &Arc<Runtime>,
    service: &Arc<Service>,
) {
    let Some(duration) = get_f64(mpv, "duration").filter(|d| d.is_finite() && *d > 0.0) else {
        return;
    };
    let path = get_string(mpv, "path");
    let mut playback = state.lock().unwrap();
    if !playback.active
        || !playback.subtitles.ready
        || path.is_none()
        || (path != playback.subtitles.original_url && path != playback.fallback_url)
    {
        return;
    }
    let Some(item) = playback
        .current_index
        .and_then(|i| playback.queue.get(i))
        .cloned()
    else {
        return;
    };
    let Some(context) = item.episode else {
        return;
    };
    if playback.segments.path == path && (playback.segments.duration - duration).abs() < 0.5 {
        return;
    }
    let chapters: Vec<_> = (0..mpv
        .get_property::<i64>("chapter-list/count")
        .unwrap_or(0)
        .clamp(0, 1024))
        .filter_map(|i| {
            Some((
                get_string(mpv, &format!("chapter-list/{i}/title")).unwrap_or_default(),
                get_f64(mpv, &format!("chapter-list/{i}/time"))?,
            ))
        })
        .collect();
    let local = markers::chapters(&chapters, duration);
    playback.segments.reset();
    playback.segments.duration = duration;
    playback.segments.position = get_f64(mpv, "time-pos").unwrap_or(0.0);
    playback.segments.path = path;
    playback.segments.segments = local.clone();
    let epoch = playback.segments.epoch;
    let state_job = state.clone();
    let weak_job = weak.clone();
    let service = service.clone();
    let task = rt.spawn(async move {
        let result = service
            .resolve(item.file_id, &context, duration, local)
            .await;
        let mut playback = state_job.lock().unwrap();
        if !playback.segments.accepts(epoch) {
            return;
        }
        playback.segments.segments = result;
        drop(playback);
        publish(&weak_job, &state_job);
    });
    playback.segments.job = Some(task.abort_handle());
    drop(playback);
    publish(weak, state);
}

pub(super) fn position(
    weak: &slint::Weak<AppWindow>,
    state: &Arc<Mutex<PlayerPlaybackState>>,
    position: f64,
) {
    let changed = {
        let mut playback = state.lock().unwrap();
        let session = &mut playback.segments;
        let before = session.active().map(|s| (s.kind, s.start, s.end));
        session.position = position;
        before != session.active().map(|s| (s.kind, s.start, s.end))
    };
    if changed {
        publish(weak, state);
    }
}

fn publish(weak: &slint::Weak<AppWindow>, state: &Arc<Mutex<PlayerPlaybackState>>) {
    let epoch = state.lock().unwrap().segments.epoch;
    let state = state.clone();
    let _ = weak.upgrade_in_event_loop(move |app| {
        let playback = state.lock().unwrap();
        if playback.segments.epoch != epoch {
            return;
        }
        let active = playback
            .active
            .then(|| playback.segments.active())
            .flatten();
        let label = match active.map(|s| s.kind) {
            Some(Kind::Intro) => "Skip intro",
            Some(Kind::Recap) => "Skip recap",
            _ => "",
        };
        let has_next = playback
            .current_index
            .and_then(|i| playback.queue.get(i + 1))
            .is_some_and(|i| i.episode.is_some());
        let next = active.is_some_and(|s| s.kind == Kind::Credits) && has_next;
        app.set_player_skip_label(label.into());
        app.set_player_skip_next_available(next);
        app.set_player_skip_source(active.map(|s| s.source.as_str()).unwrap_or_default().into());
    });
}

pub(super) fn register(
    app: &AppWindow,
    engine: Arc<PlayerEngine>,
    state: Arc<Mutex<PlayerPlaybackState>>,
    watch: Arc<WatchSyncService>,
) {
    let skip_state = state.clone();
    let weak = app.as_weak();
    app.on_player_skip_segment(move || {
        let playback = skip_state.lock().unwrap();
        if !playback.active {
            return;
        }
        let Some(segment) = playback
            .segments
            .active()
            .filter(|s| s.kind != Kind::Credits)
        else {
            return;
        };
        let end = segment.end.min(playback.segments.duration);
        drop(playback);
        if engine.seek(end).is_ok() {
            if let Some(app) = weak.upgrade() {
                reset_ui(&app);
            }
        }
    });
    let weak = app.as_weak();
    app.on_player_skip_next(move || {
        let playback = state.lock().unwrap();
        if !playback.active {
            return;
        }
        let Some(credits) = playback
            .segments
            .active()
            .filter(|s| s.kind == Kind::Credits)
        else {
            return;
        };
        let Some(index) = playback.current_index else {
            return;
        };
        if !playback
            .queue
            .get(index + 1)
            .is_some_and(|i| i.episode.is_some())
        {
            return;
        }
        let file_id = playback.queue[index].file_id;
        let finished = credits.terminal(playback.segments.duration);
        drop(playback);
        if finished {
            watch.mark_watched(file_id, true);
        }
        if let Some(app) = weak.upgrade() {
            app.invoke_player_playlist_next();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_episode_invalidates_results_and_active_markers() {
        let mut session = Session {
            path: Some("file-a".into()),
            duration: 1000.0,
            position: 20.0,
            segments: vec![Segment {
                kind: Kind::Intro,
                start: 10.0,
                end: 60.0,
                source: "test".into(),
            }],
            ..Default::default()
        };
        let epoch = session.epoch;
        assert!(session.accepts(epoch));
        assert!(session.active().is_some());
        session.reset();
        session.path = Some("file-b".into());
        assert!(!session.accepts(epoch));
        assert!(session.active().is_none());
    }
}
