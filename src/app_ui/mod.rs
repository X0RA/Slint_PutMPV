//! UI bridge: Slint callbacks, view models, and presentation helpers. Domain modules stay free of
//! generated Slint types.

pub(crate) mod auth;
pub(crate) mod files;
pub(crate) mod media;
pub(crate) mod metadata_ui;
pub(crate) mod models;
pub(crate) mod settings;
pub(crate) mod state;
pub(crate) mod tv_show;
pub(crate) mod util;

use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle;
use tokio::runtime::Runtime;

use crate::metadata as meta_api;
use crate::player;
use crate::putio::PutioClient;
use crate::storage::config::ConfigStore;
use crate::storage::file_state::FileStateStore;
use crate::storage::files_store::FilesStore;
use crate::storage::matched_store::MatchedStore;
use crate::storage::tmdb_store::TMDBStore;
use crate::storage::tvmaze_store::TVMazeStore;
use crate::AppWindow;

pub(crate) use models::UiModels;
pub(crate) use state::UiState;

pub(crate) const VIEW_LOADING: i32 = 0;
pub(crate) const VIEW_SPLASH: i32 = 1;
pub(crate) const VIEW_CODE: i32 = 2;
pub(crate) const VIEW_FILES: i32 = 3;
pub(crate) const VIEW_MEDIA: i32 = 4;
pub(crate) const VIEW_TV_SHOW: i32 = 8;
pub(crate) const VIEW_PLAYER: i32 = 7;
pub(crate) const VIEW_SPLASH_AFTER_RESET: i32 = VIEW_SPLASH;

/// Runtime services and API clients (not Slint-specific).
pub(crate) struct Services {
    pub config: Arc<ConfigStore>,
    pub files_store: Arc<FilesStore>,
    pub matched_store: Arc<MatchedStore>,
    pub tmdb_store: Arc<TMDBStore>,
    pub tvmaze_store: Arc<TVMazeStore>,
    pub metadata_api: Arc<meta_api::MetadataAPI>,
    pub tmdb_api: Arc<meta_api::TMDBAPI>,
    pub tvmaze_api: Arc<meta_api::TVMazeAPI>,
    pub file_state: Arc<std::sync::RwLock<FileStateStore>>,
    pub watch_sync: Arc<crate::sync::watch_session::WatchSyncService>,
    pub client: PutioClient,
    pub rt: Arc<Runtime>,
}

/// Everything needed to register UI callbacks for one `AppWindow` instance.
pub(crate) struct UiCtx {
    pub services: Services,
    pub state: UiState,
    pub models: UiModels,
    pub embedded_player: player::EmbeddedPlayer,
}

pub(crate) fn install(app: &AppWindow, ctx: &UiCtx) {
    let weak = app.as_weak();
    let request_refresh: Rc<dyn Fn()> = Rc::new({
        let weak = weak.clone();
        move || {
            if let Some(a) = weak.upgrade() {
                a.invoke_request_refresh();
            }
        }
    });

    let metadata_refresh: Rc<dyn Fn()> = Rc::new({
        let weak = weak.clone();
        let metadata_model = ctx.models.metadata.clone();
        let metadata_state = ctx.state.metadata_state.clone();
        let tree = ctx.state.tree.clone();
        let matched_store = ctx.services.matched_store.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                self::metadata_ui::refresh_metadata_ui(
                    &app,
                    &metadata_model,
                    &metadata_state,
                    &tree,
                    &matched_store,
                );
            }
        }
    });

    let media_refresh: Rc<dyn Fn()> = Rc::new({
        let weak = weak.clone();
        let media_movies = ctx.models.media_movies.clone();
        let media_shows = ctx.models.media_shows.clone();
        let tree = ctx.state.tree.clone();
        let matched_store = ctx.services.matched_store.clone();
        let tmdb_store = ctx.services.tmdb_store.clone();
        let file_state = ctx.services.file_state.clone();
        let rt = ctx.services.rt.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let missing = self::media::refresh_media_ui(
                &app,
                &media_movies,
                &media_shows,
                &tree,
                &matched_store,
                &tmdb_store,
                &file_state,
            );
            if !missing.is_empty() {
                let weak = weak.clone();
                rt.spawn(async move {
                    self::media::download_posters(missing).await;
                    let _ = weak.upgrade_in_event_loop(|app| {
                        app.invoke_media_refresh();
                    });
                });
            }
        }
    });

    files::install(
        app,
        &ctx.state,
        &ctx.models,
        &ctx.services,
        request_refresh.clone(),
        &ctx.services.rt,
        &ctx.embedded_player,
    );

    settings::install(
        app,
        &ctx.state,
        &ctx.services,
        request_refresh,
        &ctx.services.rt,
    );

    auth::install(app, &ctx.services, &ctx.state, &ctx.services.rt);

    metadata_ui::install(
        app,
        metadata_refresh.clone(),
        &ctx.services,
        &ctx.state,
        &ctx.models,
        &ctx.services.rt,
    );

    media::install(
        app,
        media_refresh,
        &ctx.services,
        &ctx.state,
        &ctx.models,
        &ctx.services.rt,
        &ctx.embedded_player,
    );

    tv_show::install(
        app,
        &ctx.services,
        &ctx.state,
        &ctx.models,
        &ctx.services.rt,
        &ctx.embedded_player,
    );
}
