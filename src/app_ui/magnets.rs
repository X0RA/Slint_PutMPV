use std::rc::Rc;
use std::sync::{Arc, Mutex};

use magneto::{Knaben, Magneto, SearchRequest, Torrent};
use slint::{ComponentHandle, ModelRc, VecModel};
use tokio::runtime::Runtime;
use tracing::warn;

use crate::app_ui::util::format_size;
use crate::putio;
use crate::{AppWindow, MagnetItem};

use super::{Services, UiState};

#[derive(Default)]
struct MagnetsCache {
    results: Vec<Torrent>,
}

pub(crate) fn install(app: &AppWindow, services: &Services, _state: &UiState, rt: &Arc<Runtime>) {
    let cache: Arc<Mutex<MagnetsCache>> = Arc::new(Mutex::new(MagnetsCache::default()));

    app.on_magnets_search({
        let weak = app.as_weak();
        let rt = rt.clone();
        let cache = cache.clone();
        move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let query = app.get_magnets_query().trim().to_string();
            if query.is_empty() {
                cache.lock().unwrap().results.clear();
                app.set_magnets_items(empty_model());
                app.set_magnets_count_label("0 results".into());
                app.set_magnets_status("Type a search and press Enter.".into());
                return;
            }
            app.set_magnets_busy(true);
            app.set_magnets_status("".into());
            app.set_magnets_count_label("Searching...".into());

            let weak = weak.clone();
            let cache = cache.clone();
            rt.spawn(async move {
                let m = Magneto::with_providers(vec![Box::new(Knaben::new())]);
                let result = m.search(SearchRequest::new(&query)).await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_magnets_busy(false);
                    match result {
                        Ok(torrents) => {
                            cache.lock().unwrap().results = torrents;
                            apply_view(&app, &cache);
                            if app.get_magnets_status().starts_with("Could not") {
                                app.set_magnets_status("".into());
                            }
                        }
                        Err(e) => {
                            cache.lock().unwrap().results.clear();
                            app.set_magnets_items(empty_model());
                            app.set_magnets_count_label("0 results".into());
                            app.set_magnets_status(format!("Could not search: {e}").into());
                        }
                    }
                });
            });
        }
    });

    app.on_magnets_copy({
        let weak = app.as_weak();
        move |magnet| {
            let s = magnet.to_string();
            if s.is_empty() {
                return;
            }
            match arboard::Clipboard::new() {
                Ok(mut cb) => match cb.set_text(s) {
                    Ok(()) => {
                        if let Some(app) = weak.upgrade() {
                            app.set_magnets_status("Magnet copied.".into());
                        }
                    }
                    Err(e) => {
                        warn!("clipboard write failed: {e}");
                        if let Some(app) = weak.upgrade() {
                            app.set_magnets_status(format!("Could not copy: {e}").into());
                        }
                    }
                },
                Err(e) => {
                    warn!("clipboard init failed: {e}");
                    if let Some(app) = weak.upgrade() {
                        app.set_magnets_status(format!("Could not copy: {e}").into());
                    }
                }
            }
        }
    });

    app.on_magnets_download({
        let weak = app.as_weak();
        let client = services.client.clone();
        let config = services.config.clone();
        let rt = rt.clone();
        move |magnet| {
            let magnet = magnet.to_string();
            if magnet.is_empty() {
                return;
            }
            let Some(app) = weak.upgrade() else {
                return;
            };
            let token = config.oauth_token();
            if token.is_empty() {
                app.set_magnets_status("Sign in before adding transfers.".into());
                return;
            }
            app.set_magnets_status("Adding transfer...".into());
            let weak = weak.clone();
            let client = client.clone();
            rt.spawn(async move {
                let message = match putio::transfers::add_url(&client, &token, &magnet).await {
                    Ok(_) => "Transfer added.".to_string(),
                    Err(e) => format!("Could not add transfer: {e}"),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_magnets_status(message.into());
                });
            });
        }
    });

    app.on_magnets_filter_changed({
        let weak = app.as_weak();
        let cache = cache.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                apply_view(&app, &cache);
            }
        }
    });

    app.on_magnets_sort_changed({
        let weak = app.as_weak();
        let cache = cache.clone();
        move |_idx| {
            if let Some(app) = weak.upgrade() {
                apply_view(&app, &cache);
            }
        }
    });

    app.on_magnets_sort_direction_toggled({
        let weak = app.as_weak();
        let cache = cache.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                apply_view(&app, &cache);
            }
        }
    });
}

fn empty_model() -> ModelRc<MagnetItem> {
    ModelRc::from(Rc::new(VecModel::<MagnetItem>::from(Vec::<MagnetItem>::new())))
}

fn apply_view(app: &AppWindow, cache: &Arc<Mutex<MagnetsCache>>) {
    let cache = cache.lock().unwrap();
    let filters = collect_filters(app);
    let mut filtered: Vec<&Torrent> = cache
        .results
        .iter()
        .filter(|t| matches_filters(&t.name, &filters))
        .collect();
    sort_view(
        &mut filtered,
        app.get_magnets_sort_index(),
        app.get_magnets_sort_descending(),
    );
    let count = filtered.len();
    let items: Vec<MagnetItem> = filtered.iter().map(|t| torrent_to_item(t)).collect();
    app.set_magnets_items(ModelRc::from(Rc::new(VecModel::from(items))));
    app.set_magnets_count_label(
        format!("{count} result{}", if count == 1 { "" } else { "s" }).into(),
    );
}

struct Filters {
    chips: Vec<&'static str>,
}

fn collect_filters(app: &AppWindow) -> Filters {
    let mut chips = Vec::new();
    if app.get_magnets_f720p() {
        chips.push("720p");
    }
    if app.get_magnets_f1080p() {
        chips.push("1080p");
    }
    if app.get_magnets_f2160p() {
        chips.push("2160p");
    }
    if app.get_magnets_fx264() {
        chips.push("x264");
    }
    if app.get_magnets_fx265() {
        chips.push("x265");
    }
    if app.get_magnets_fhdr() {
        chips.push("hdr");
    }
    Filters { chips }
}

fn matches_filters(title: &str, filters: &Filters) -> bool {
    if filters.chips.is_empty() {
        return true;
    }
    let lower = title.to_lowercase();
    filters.chips.iter().all(|needle| lower.contains(needle))
}

fn sort_view(items: &mut [&Torrent], sort_index: i32, descending: bool) {
    use std::cmp::Ordering;
    items.sort_by(|a, b| {
        let ord = match sort_index {
            0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            1 => a.seeders.cmp(&b.seeders),
            2 => a.size_bytes.cmp(&b.size_bytes),
            3 => a.peers.cmp(&b.peers),
            4 => a.provider.to_lowercase().cmp(&b.provider.to_lowercase()),
            _ => Ordering::Equal,
        };
        if descending {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn torrent_to_item(t: &Torrent) -> MagnetItem {
    MagnetItem {
        title: t.name.clone().into(),
        source: t.provider.clone().into(),
        size: format_size(t.size_bytes).into(),
        seeders: format_number(t.seeders).into(),
        peers: format_number(t.peers).into(),
        magnet_link: t.magnet_link.clone().into(),
    }
}

fn format_number(n: u32) -> String {
    let raw = n.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (i, c) in raw.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatted_with_commas() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(42), "42");
        assert_eq!(format_number(1_247), "1,247");
        assert_eq!(format_number(1_000_000), "1,000,000");
    }

    #[test]
    fn filters_are_anded() {
        let filters = Filters {
            chips: vec!["2160p", "hdr"],
        };
        assert!(matches_filters(
            "Movie.2024.2160p.WEB-DL.HDR.x265",
            &filters
        ));
        assert!(!matches_filters("Movie.2024.2160p.WEB-DL.x265", &filters));
        assert!(!matches_filters("Movie.2024.1080p.HDR.x265", &filters));
    }

    #[test]
    fn empty_filters_pass_everything() {
        let filters = Filters { chips: vec![] };
        assert!(matches_filters("anything", &filters));
    }
}
