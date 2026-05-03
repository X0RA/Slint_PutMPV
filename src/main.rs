use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::runtime::Runtime;

use putio::PutioClient;
use storage::config::ConfigStore;
use storage::file_state::FileStateStore;
use storage::files_store::FilesStore;
use storage::matched_store::MatchedStore;
use storage::tmdb_store::TMDBStore;
use storage::tvmaze_store::TVMazeStore;

mod app_ui;
mod fileparser;
mod metadata;
mod mpv;
mod player;
mod putio;
mod storage;

slint::include_modules!();

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,putmpv=debug".into()),
        )
        .init();

    let config = Arc::new(ConfigStore::load()?);
    let files_store = Arc::new(FilesStore::load()?);
    let matched_store = Arc::new(MatchedStore::load()?);
    let tmdb_store = Arc::new(TMDBStore::load()?);
    let tvmaze_store = Arc::new(TVMazeStore::load()?);
    let tmdb_api = Arc::new(metadata::TMDBAPI::new(config.clone(), tmdb_store.clone()));
    let tvmaze_api = Arc::new(metadata::TVMazeAPI::new(tvmaze_store.clone()));
    let metadata_api = Arc::new(metadata::MetadataAPI::new(
        matched_store.clone(),
        tmdb_api.clone(),
        tvmaze_api.clone(),
    ));
    let file_state = Arc::new(RwLock::new(FileStateStore::load()?));
    let client = PutioClient::new();
    let rt = Arc::new(Runtime::new()?);

    let app = AppWindow::new()?;
    app.set_files_mode(config.files_mode());
    app.set_files_sort(config.files_sort());
    app.set_files_sort_descending(config.files_sort_descending());
    app.set_view(app_ui::VIEW_LOADING);
    app.set_loading_message("Checking sign-in…".into());

    let embedded_player = player::EmbeddedPlayer::install(
        &app,
        client.clone(),
        config.clone(),
        rt.clone(),
        app_ui::VIEW_PLAYER,
        app_ui::VIEW_FILES,
    );

    let models = app_ui::UiModels::new_and_bind(&app);
    let state = app_ui::UiState::new();

    app.set_detail_item(app_ui::files::empty_file_item());

    let services = app_ui::Services {
        config,
        files_store,
        matched_store,
        tmdb_store,
        tvmaze_store,
        metadata_api,
        tmdb_api,
        tvmaze_api,
        file_state,
        client,
        rt: rt.clone(),
    };

    let ctx = app_ui::UiCtx {
        services,
        state,
        models,
        embedded_player,
    };

    app_ui::install(&app, &ctx);
    app_ui::auth::run_startup(&app, &ctx.services, &ctx.state, &ctx.services.rt);

    app.run()?;
    drop(rt);
    Ok(())
}
