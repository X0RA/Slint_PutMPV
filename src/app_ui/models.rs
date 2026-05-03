//! Slint `VecModel` handles bound to `AppWindow` properties.

use std::rc::Rc;

use slint::{ModelRc, VecModel};

use crate::{
    AppWindow, FileItem, MediaItem, MetadataItem, PathSegment, TvCreatorChip, TvEpisodeRow,
    TvHeroBadge, TvSeasonTab,
};

pub(crate) struct UiModels {
    pub visible: Rc<VecModel<FileItem>>,
    pub path: Rc<VecModel<PathSegment>>,
    pub metadata: Rc<VecModel<MetadataItem>>,
    pub media_movies: Rc<VecModel<MediaItem>>,
    pub media_shows: Rc<VecModel<MediaItem>>,
    pub tv_seasons: Rc<VecModel<TvSeasonTab>>,
    pub tv_episodes: Rc<VecModel<TvEpisodeRow>>,
    pub tv_hero_badges: Rc<VecModel<TvHeroBadge>>,
    pub tv_hero_creators: Rc<VecModel<TvCreatorChip>>,
    pub tv_detail_lines: Rc<VecModel<slint::SharedString>>,
    pub tv_networks: Rc<VecModel<slint::SharedString>>,
}

impl UiModels {
    pub(crate) fn new_and_bind(app: &AppWindow) -> Self {
        let visible = Rc::new(VecModel::from(Vec::<FileItem>::new()));
        let path = Rc::new(VecModel::from(Vec::<PathSegment>::new()));
        let metadata = Rc::new(VecModel::from(Vec::<MetadataItem>::new()));
        let media_movies = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
        let media_shows = Rc::new(VecModel::from(Vec::<MediaItem>::new()));
        let tv_seasons = Rc::new(VecModel::from(Vec::<TvSeasonTab>::new()));
        let tv_episodes = Rc::new(VecModel::from(Vec::<TvEpisodeRow>::new()));
        let tv_hero_badges = Rc::new(VecModel::from(Vec::<TvHeroBadge>::new()));
        let tv_hero_creators = Rc::new(VecModel::from(Vec::<TvCreatorChip>::new()));
        let tv_detail_lines = Rc::new(VecModel::from(Vec::<slint::SharedString>::new()));
        let tv_networks = Rc::new(VecModel::from(Vec::<slint::SharedString>::new()));

        app.set_visible_items(ModelRc::from(visible.clone()));
        app.set_path_segments(ModelRc::from(path.clone()));
        app.set_metadata_items(ModelRc::from(metadata.clone()));
        app.set_media_movies(ModelRc::from(media_movies.clone()));
        app.set_media_shows(ModelRc::from(media_shows.clone()));
        app.set_tv_show_seasons(ModelRc::from(tv_seasons.clone()));
        app.set_tv_show_episodes(ModelRc::from(tv_episodes.clone()));
        app.set_tv_show_hero_badges(ModelRc::from(tv_hero_badges.clone()));
        app.set_tv_show_hero_creators(ModelRc::from(tv_hero_creators.clone()));
        app.set_tv_show_detail_lines(ModelRc::from(tv_detail_lines.clone()));
        app.set_tv_show_networks(ModelRc::from(tv_networks.clone()));

        Self {
            visible,
            path,
            metadata,
            media_movies,
            media_shows,
            tv_seasons,
            tv_episodes,
            tv_hero_badges,
            tv_hero_creators,
            tv_detail_lines,
            tv_networks,
        }
    }
}
