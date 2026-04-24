use std::cell::RefCell;
use std::rc::Rc;

use slint::{ModelRc, VecModel};

slint::include_modules!();

#[derive(Clone)]
struct FsItem {
    id: i32,
    item_type: &'static str,
    name: &'static str,
    kind: &'static str,
    updated_abs: &'static str,
    list_size: &'static str,
    grid_meta: &'static str,
    detail_size: &'static str,
    detail_kind: &'static str,
    extra_a_label: &'static str,
    extra_a_value: &'static str,
    extra_b_label: &'static str,
    extra_b_value: &'static str,
    is_media: bool,
}

#[derive(Clone)]
struct MdItem {
    id: i32,
    media_type: &'static str,
    title: &'static str,
    subtitle: &'static str,
    matched: bool,
    expanded: bool,
}

fn empty_file_item() -> FileItem {
    FileItem {
        id: -1,
        item_type: "".into(),
        name: "".into(),
        kind: "".into(),
        grid_meta: "".into(),
        list_kind: "".into(),
        list_size: "".into(),
        list_updated: "".into(),
        detail_size: "".into(),
        detail_kind: "".into(),
        detail_extra_a_label: "".into(),
        detail_extra_a_value: "".into(),
        detail_extra_b_label: "".into(),
        detail_extra_b_value: "".into(),
        location: "".into(),
        is_media: false,
    }
}

fn file_item(item: &FsItem, location: &str) -> FileItem {
    FileItem {
        id: item.id,
        item_type: item.item_type.into(),
        name: item.name.into(),
        kind: item.kind.into(),
        grid_meta: item.grid_meta.into(),
        list_kind: if item.item_type == "folder" {
            "Folder".into()
        } else if item.kind == "movie" || item.kind == "tv" {
            "Media".into()
        } else {
            item.kind.into()
        },
        list_size: item.list_size.into(),
        list_updated: item.updated_abs.into(),
        detail_size: item.detail_size.into(),
        detail_kind: item.detail_kind.into(),
        detail_extra_a_label: item.extra_a_label.into(),
        detail_extra_a_value: item.extra_a_value.into(),
        detail_extra_b_label: item.extra_b_label.into(),
        detail_extra_b_value: item.extra_b_value.into(),
        location: location.into(),
        is_media: item.is_media,
    }
}

fn metadata_item(item: &MdItem, selected: bool) -> MetadataItem {
    let badge = if item.matched { "Matched" } else { "Unmatched" };
    MetadataItem {
        id: item.id,
        media_type: item.media_type.into(),
        title: item.title.into(),
        subtitle: item.subtitle.into(),
        badge: badge.into(),
        matched: item.matched,
        expanded: item.expanded,
        selected,
    }
}

fn mock_root_items() -> Vec<FsItem> {
    vec![
        FsItem { id: 1, item_type: "folder", name: "Lessons Of Darkness (1992) [1080p] [BluRay] [YTS.MX]", kind: "movie", updated_abs: "Mar 19, 2026", list_size: "5 items", grid_meta: "5 items · 1w ago", detail_size: "5 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 2, item_type: "folder", name: "Manhattan (1979) [BluRay] [1080p] [YTS.AM]", kind: "movie", updated_abs: "Mar 19, 2026", list_size: "2 items", grid_meta: "2 items · 1w ago", detail_size: "2 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 3, item_type: "folder", name: "Female Prisoner #701 Scorpion (1972) [1080p] [BluRay]", kind: "movie", updated_abs: "Mar 18, 2026", list_size: "3 items", grid_meta: "3 items · 1w ago", detail_size: "3 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 4, item_type: "folder", name: "The.Incredibles.2004.1080p.BluRay.ETRG", kind: "movie", updated_abs: "Mar 14, 2026", list_size: "5 items", grid_meta: "5 items · 1w ago", detail_size: "5 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 5, item_type: "folder", name: "The Human Condition II Road To Eternity (1959)", kind: "movie", updated_abs: "Mar 14, 2026", list_size: "2 items", grid_meta: "2 items · 1w ago", detail_size: "2 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 6, item_type: "folder", name: "Wrath Of Man (2021) [1080p] [WEBRip] [5.1]", kind: "movie", updated_abs: "Mar 12, 2026", list_size: "4 items", grid_meta: "4 items · 1w ago", detail_size: "4 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 7, item_type: "file", name: "Interstellar.2014.IMAX.2160p.HDR.mkv", kind: "movie", updated_abs: "Mar 11, 2026", list_size: "64.20 GB", grid_meta: "64.20 GB · 1w ago", detail_size: "64.20 GB", detail_kind: "MOVIE", extra_a_label: "Duration", extra_a_value: "2h 49m", extra_b_label: "Resolution", extra_b_value: "3840x1600", is_media: true },
        FsItem { id: 8, item_type: "folder", name: "Vij Eller Praestseminariets Likvaka (1967) [1080p] [BluRay]", kind: "movie", updated_abs: "Mar 09, 2026", list_size: "3 items", grid_meta: "3 items · 2w ago", detail_size: "3 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 9, item_type: "file", name: "Aphex Twin - Selected Ambient Works 85-92.flac", kind: "music", updated_abs: "Mar 07, 2026", list_size: "484 MB", grid_meta: "484 MB · 2w ago", detail_size: "484 MB", detail_kind: "MUSIC", extra_a_label: "Duration", extra_a_value: "1h 14m", extra_b_label: "", extra_b_value: "", is_media: true },
        FsItem { id: 10, item_type: "folder", name: "One-Armed Boxer (1972) [720p] [YTS.MX]", kind: "movie", updated_abs: "Mar 06, 2026", list_size: "3 items", grid_meta: "3 items · 2w ago", detail_size: "3 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 11, item_type: "folder", name: "Thou - Blessings of the Highest Order (2020) FLAC", kind: "music", updated_abs: "Mar 04, 2026", list_size: "17 items", grid_meta: "17 items · 2w ago", detail_size: "17 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 12, item_type: "folder", name: "Surf's Up (2007) [1080p] [BluRay] [5.1] [YTS.MX]", kind: "movie", updated_abs: "Mar 03, 2026", list_size: "3 items", grid_meta: "3 items · 2w ago", detail_size: "3 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 13, item_type: "file", name: "Sentenced to Be a Hero - S01E01.mp4", kind: "tv", updated_abs: "Mar 03, 2026", list_size: "1.80 GB", grid_meta: "1.80 GB · 2w ago", detail_size: "1.80 GB", detail_kind: "TV", extra_a_label: "Duration", extra_a_value: "24m", extra_b_label: "", extra_b_value: "", is_media: true },
        FsItem { id: 14, item_type: "folder", name: "Community Season 2 [1080p x265 10bit FS83 Joy]", kind: "tv", updated_abs: "Mar 01, 2026", list_size: "29 items", grid_meta: "29 items · 3w ago", detail_size: "29 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 15, item_type: "file", name: "Amelie.2001.1080p.BRRip.AC3.x265.mkv", kind: "movie", updated_abs: "Feb 28, 2026", list_size: "2.40 GB", grid_meta: "2.40 GB · 3w ago", detail_size: "2.40 GB", detail_kind: "MOVIE", extra_a_label: "Duration", extra_a_value: "2h 02m", extra_b_label: "Resolution", extra_b_value: "1920x1080", is_media: true },
        FsItem { id: 16, item_type: "folder", name: "Crumb (1994) [1080p] [BluRay] [YTS.MX]", kind: "movie", updated_abs: "Feb 27, 2026", list_size: "3 items", grid_meta: "3 items · 3w ago", detail_size: "3 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 17, item_type: "folder", name: "Magazine Dreams (2023) [1080p] [WEBRip] [5.1]", kind: "movie", updated_abs: "Feb 27, 2026", list_size: "5 items", grid_meta: "5 items · 3w ago", detail_size: "5 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 18, item_type: "folder", name: "[Sokudo] Jujutsu Kaisen - S02 [1080p BD AV1]", kind: "tv", updated_abs: "Feb 20, 2026", list_size: "23 items", grid_meta: "23 items · 1mo ago", detail_size: "23 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 19, item_type: "file", name: "pilot-notes-march.pdf", kind: "document", updated_abs: "Feb 19, 2026", list_size: "1.20 MB", grid_meta: "1.20 MB · 1mo ago", detail_size: "1.20 MB", detail_kind: "DOCUMENT", extra_a_label: "Pages", extra_a_value: "18", extra_b_label: "", extra_b_value: "", is_media: false },
        FsItem { id: 20, item_type: "folder", name: "Harakiri (1962) (JAPANESE) ENG SUBS 1080p", kind: "movie", updated_abs: "Feb 14, 2026", list_size: "4 items", grid_meta: "4 items · 1mo ago", detail_size: "4 items", detail_kind: "FOLDER", extra_a_label: "", extra_a_value: "", extra_b_label: "", extra_b_value: "", is_media: false },
    ]
}

fn location_text(stack: &[(i32, String)]) -> String {
    stack.iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn children_for_folder(root_items: &[FsItem], folder_id: i32) -> Vec<FsItem> {
    if folder_id == 0 {
        return root_items.to_vec();
    }

    let seed = ((folder_id.unsigned_abs() as usize) * 3) % (root_items.len().saturating_sub(5).max(1));
    let end = (seed + 5).min(root_items.len());

    root_items[seed..end]
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let mut next = item.clone();
            next.id = folder_id * 100 + idx as i32 + 1;
            next
        })
        .collect()
}

fn main() -> Result<(), slint::PlatformError> {
    let app = AppWindow::new()?;

    let root_items = Rc::new(mock_root_items());
    let metadata_source = Rc::new(RefCell::new(vec![
        MdItem { id: 100, media_type: "TV", title: "Legend of the Galactic Heroes", subtitle: "1 season · 75 episodes · 1988", matched: true, expanded: false },
        MdItem { id: 101, media_type: "TV", title: "Naruto", subtitle: "1 season · 720 episodes · 2002", matched: true, expanded: false },
        MdItem { id: 102, media_type: "TV", title: "What We Do in the Shadows", subtitle: "1 season · 10 episodes · 2019", matched: true, expanded: false },
        MdItem { id: 200, media_type: "Movie", title: "Avatar Aang The Last Airbender", subtitle: "Avatar.Aang.The.Last.Airbender.1080p.BluRay.mkv", matched: false, expanded: false },
        MdItem { id: 201, media_type: "Movie", title: "Goyokin", subtitle: "Goyokin.1969.1080p.BluRay.x264.AAC.mkv", matched: true, expanded: false },
        MdItem { id: 202, media_type: "Movie", title: "Split Second", subtitle: "Split.Second.1992.1080p.BluRay.x264.mp4", matched: true, expanded: false },
        MdItem { id: 203, media_type: "Movie", title: "Labyrinth", subtitle: "Labyrinth.1986.1080p.BluRay.x264.mp4", matched: true, expanded: false },
    ]));
    let metadata_selected = Rc::new(RefCell::new(Vec::<i32>::new()));

    let current_folder = Rc::new(RefCell::new(0_i32));
    let path_stack = Rc::new(RefCell::new(vec![(0_i32, String::from("chill.institute"))]));

    let visible_model = Rc::new(VecModel::from(Vec::<FileItem>::new()));
    let path_model = Rc::new(VecModel::from(Vec::<PathSegment>::new()));
    let metadata_model = Rc::new(VecModel::from(Vec::<MetadataItem>::new()));
    app.set_visible_items(ModelRc::from(visible_model.clone()));
    app.set_path_segments(ModelRc::from(path_model.clone()));
    app.set_metadata_items(ModelRc::from(metadata_model.clone()));
    app.set_detail_item(empty_file_item());

    let refresh_files = {
        let app = app.as_weak();
        let root_items = root_items.clone();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let visible_model = visible_model.clone();
        let path_model = path_model.clone();
        move || {
            let Some(app) = app.upgrade() else { return; };
            let folder_id = *current_folder.borrow();
            let location = location_text(&path_stack.borrow());
            let mut rows = children_for_folder(&root_items, folder_id);
            let query = app.get_files_query().to_lowercase();

            rows.retain(|item| query.is_empty() || item.name.to_lowercase().contains(&query));

            match app.get_files_sort() {
                1 => rows.sort_by(|a, b| a.name.cmp(b.name)),
                2 => rows.sort_by(|a, b| b.list_size.cmp(a.list_size)),
                3 => rows.sort_by(|a, b| b.updated_abs.cmp(a.updated_abs)),
                _ => rows.sort_by(|a, b| b.updated_abs.cmp(a.updated_abs)),
            }

            let folder_count = rows.iter().filter(|item| item.item_type == "folder").count() as i32;
            let file_count = rows.len() as i32 - folder_count;

            visible_model.set_vec(
                rows.iter()
                    .map(|item| file_item(item, &location))
                    .collect::<Vec<_>>(),
            );
            path_model.set_vec(
                path_stack
                    .borrow()
                    .iter()
                    .skip(1)
                    .map(|(id, name)| PathSegment { id: *id, name: name.clone().into() })
                    .collect::<Vec<_>>(),
            );

            app.set_folder_count(folder_count);
            app.set_file_count(file_count);
            app.set_has_parent(path_stack.borrow().len() > 1);
            app.set_total_label("TOTAL · 1.84 TB".into());
        }
    };

    let refresh_metadata = {
        let app = app.as_weak();
        let metadata_source = metadata_source.clone();
        let metadata_selected = metadata_selected.clone();
        let metadata_model = metadata_model.clone();
        move || {
            let Some(app) = app.upgrade() else { return; };
            let query = app.get_metadata_query().to_lowercase();
            let filter = app.get_metadata_filter();
            let hide_matched = app.get_metadata_hide_matched();
            let selected = metadata_selected.borrow();
            let source = metadata_source.borrow();
            let visible = source
                .iter()
                .filter(|item| match filter {
                    1 => item.media_type == "TV",
                    2 => item.media_type == "Movie",
                    _ => true,
                })
                .filter(|item| !hide_matched || !item.matched)
                .filter(|item| query.is_empty() || item.title.to_lowercase().contains(&query))
                .cloned()
                .collect::<Vec<_>>();

            let tv_count = visible.iter().filter(|item| item.media_type == "TV").count() as i32;
            let movie_count = visible.iter().filter(|item| item.media_type == "Movie").count() as i32;
            let unmatched_count = source.iter().filter(|item| !item.matched).count() as i32;
            let selected_count = selected.len() as i32;

            metadata_model.set_vec(
                visible
                    .iter()
                    .map(|item| metadata_item(item, selected.contains(&item.id)))
                    .collect::<Vec<_>>(),
            );
            app.set_metadata_total_count(source.len() as i32);
            app.set_metadata_selected_count(selected_count);
            app.set_metadata_unmatched_count(unmatched_count);
            app.set_metadata_tv_count(tv_count);
            app.set_metadata_movie_count(movie_count);
        }
    };

    refresh_files();
    refresh_metadata();

    app.on_sign_in({
        let app = app.as_weak();
        move || {
            if let Some(app) = app.upgrade() {
                app.set_view(1);
            }
        }
    });
    app.on_code_back({
        let app = app.as_weak();
        move || {
            if let Some(app) = app.upgrade() {
                app.set_view(0);
            }
        }
    });
    app.on_code_open_link(|| {
        println!("Open browser link requested");
    });
    app.on_code_continue({
        let app = app.as_weak();
        move || {
            if let Some(app) = app.upgrade() {
                app.set_view(2);
            }
        }
    });

    app.on_files_sort_changed({
        let refresh_files = refresh_files.clone();
        move |_| refresh_files()
    });
    app.on_files_mode_changed(|_| {});
    app.on_files_query_changed({
        let refresh_files = refresh_files.clone();
        move || refresh_files()
    });

    app.on_files_open_item({
        let app = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let root_items = root_items.clone();
        let refresh_files = refresh_files.clone();
        move |id, item_type| {
            let Some(app) = app.upgrade() else { return; };
            if item_type.as_str() == "folder" {
                let current_items = children_for_folder(&root_items, *current_folder.borrow());
                if let Some(item) = current_items.iter().find(|entry| entry.id == id) {
                    *current_folder.borrow_mut() = id;
                    path_stack.borrow_mut().push((id, item.name.to_string()));
                    app.set_detail_open(false);
                    app.set_detail_item(empty_file_item());
                    refresh_files();
                }
                return;
            }

            let current_items = children_for_folder(&root_items, *current_folder.borrow());
            if let Some(item) = current_items.iter().find(|entry| entry.id == id) {
                let location = location_text(&path_stack.borrow());
                app.set_detail_item(file_item(item, &location));
                app.set_detail_open(true);
            }
        }
    });

    app.on_files_go_up({
        let app = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let refresh_files = refresh_files.clone();
        move || {
            let Some(app) = app.upgrade() else { return; };
            let mut stack = path_stack.borrow_mut();
            if stack.len() > 1 {
                stack.pop();
            }
            *current_folder.borrow_mut() = stack.last().map(|entry| entry.0).unwrap_or(0);
            app.set_detail_open(false);
            app.set_detail_item(empty_file_item());
            refresh_files();
        }
    });

    app.on_files_go_root({
        let app = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let refresh_files = refresh_files.clone();
        move || {
            if let Some(app) = app.upgrade() {
                path_stack.borrow_mut().truncate(1);
                *current_folder.borrow_mut() = 0;
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
                refresh_files();
            }
        }
    });

    app.on_files_go_to_path({
        let app = app.as_weak();
        let current_folder = current_folder.clone();
        let path_stack = path_stack.clone();
        let refresh_files = refresh_files.clone();
        move |index| {
            let Some(app) = app.upgrade() else { return; };
            let mut stack = path_stack.borrow_mut();
            let keep = (index as usize + 2).min(stack.len());
            stack.truncate(keep);
            *current_folder.borrow_mut() = stack.last().map(|entry| entry.0).unwrap_or(0);
            app.set_detail_open(false);
            app.set_detail_item(empty_file_item());
            refresh_files();
        }
    });

    app.on_files_close_detail({
        let app = app.as_weak();
        move || {
            if let Some(app) = app.upgrade() {
                app.set_detail_open(false);
                app.set_detail_item(empty_file_item());
            }
        }
    });

    app.on_files_menu_action(move |action, id| {
        println!("menu action: {} on {}", action, id);
    });

    app.on_metadata_toggle_item({
        let metadata_selected = metadata_selected.clone();
        let refresh_metadata = refresh_metadata.clone();
        move |id| {
            let mut selected = metadata_selected.borrow_mut();
            if let Some(position) = selected.iter().position(|entry| *entry == id) {
                selected.remove(position);
            } else {
                selected.push(id);
            }
            refresh_metadata();
        }
    });
    app.on_metadata_toggle_expand({
        let metadata_source = metadata_source.clone();
        let refresh_metadata = refresh_metadata.clone();
        move |id| {
            if let Some(item) = metadata_source.borrow_mut().iter_mut().find(|entry| entry.id == id) {
                item.expanded = !item.expanded;
            }
            refresh_metadata();
        }
    });
    app.on_metadata_select_unmatched({
        let metadata_source = metadata_source.clone();
        let metadata_selected = metadata_selected.clone();
        let refresh_metadata = refresh_metadata.clone();
        move || {
            *metadata_selected.borrow_mut() = metadata_source
                .borrow()
                .iter()
                .filter(|entry| !entry.matched)
                .map(|entry| entry.id)
                .collect();
            refresh_metadata();
        }
    });
    app.on_metadata_clear_selection({
        let metadata_selected = metadata_selected.clone();
        let refresh_metadata = refresh_metadata.clone();
        move || {
            metadata_selected.borrow_mut().clear();
            refresh_metadata();
        }
    });
    app.on_metadata_criteria_changed(move || refresh_metadata());

    app.run()
}
