//! Slint `VecModel` handles bound to `AppWindow` properties.

use std::rc::Rc;

use slint::{ModelRc, VecModel};

use crate::{
    AppWindow, FileItem, MediaHeroBadge, MediaItem, MediaResumeItem, MetadataItem, PathSegment,
    TransferItem, TvDetailItem, TvEpisodeRow, TvHeroBadge, TvSeasonTab,
};

pub(crate) struct UiModels {
    pub visible: Rc<VecModel<FileItem>>,
    pub path: Rc<VecModel<PathSegment>>,
    pub metadata: Rc<VecModel<MetadataItem>>,
    pub media_movies: Rc<VecModel<MediaItem>>,
    pub media_shows: Rc<VecModel<MediaItem>>,
    pub media_hero_badges: Rc<VecModel<MediaHeroBadge>>,
    pub media_resume: Rc<VecModel<MediaResumeItem>>,
    pub tv_seasons: Rc<VecModel<TvSeasonTab>>,
    pub tv_episodes: Rc<VecModel<TvEpisodeRow>>,
    pub tv_hero_badges: Rc<VecModel<TvHeroBadge>>,
    pub tv_detail_items: Rc<VecModel<TvDetailItem>>,
}

impl UiModels {
    pub(crate) fn new_and_bind(app: &AppWindow) -> Self {
        let visible = Rc::new(VecModel::from(Vec::<FileItem>::new()));
        let path = Rc::new(VecModel::from(Vec::<PathSegment>::new()));
        let metadata = Rc::new(VecModel::from(Vec::<MetadataItem>::new()));
        let media_movies = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
        let media_shows = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
        let media_hero_badges = Rc::new(VecModel::from(Vec::<MediaHeroBadge>::new()));
        let media_resume = Rc::new(VecModel::from(Vec::<MediaResumeItem>::new()));
        let transfers = Rc::new(VecModel::from(Vec::<TransferItem>::new()));
        let tv_seasons = Rc::new(VecModel::from(Vec::<TvSeasonTab>::new()));
        let tv_episodes = Rc::new(VecModel::from(Vec::<TvEpisodeRow>::new()));
        let tv_hero_badges = Rc::new(VecModel::from(Vec::<TvHeroBadge>::new()));
        let tv_detail_items = Rc::new(VecModel::from(Vec::<TvDetailItem>::new()));

        app.set_visible_items(ModelRc::from(visible.clone()));
        app.set_path_segments(ModelRc::from(path.clone()));
        app.set_metadata_items(ModelRc::from(metadata.clone()));
        app.set_media_movies(ModelRc::from(media_movies.clone()));
        app.set_media_shows(ModelRc::from(media_shows.clone()));
        app.set_media_hero_badges(ModelRc::from(media_hero_badges.clone()));
        app.set_media_resume_items(ModelRc::from(media_resume.clone()));
        app.set_transfers_items(ModelRc::from(transfers.clone()));
        app.set_tv_show_seasons(ModelRc::from(tv_seasons.clone()));
        app.set_tv_show_episodes(ModelRc::from(tv_episodes.clone()));
        app.set_tv_show_hero_badges(ModelRc::from(tv_hero_badges.clone()));
        app.set_tv_show_detail_items(ModelRc::from(tv_detail_items.clone()));

        Self {
            visible,
            path,
            metadata,
            media_movies,
            media_shows,
            media_hero_badges,
            media_resume,
            tv_seasons,
            tv_episodes,
            tv_hero_badges,
            tv_detail_items,
        }
    }
}
