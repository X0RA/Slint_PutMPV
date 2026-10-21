//! Movie details and lazily cached cast/crew.
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use slint::{ComponentHandle, VecModel};

use crate::metadata::tmdb::{MovieCredits, MovieDetails};
use crate::{AppWindow, MoviePerson, TvDetailItem};

use super::media::{download_posters, load_cached_poster};
use super::util::make_initials;
use super::Services;

fn joined_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut unique = Vec::new();
    for name in names.map(str::trim).filter(|name| !name.is_empty()) {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    unique.join(", ")
}

fn dollars(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::from("US $");
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

fn movie_facts(details: &MovieDetails, credits: &MovieCredits) -> Vec<TvDetailItem> {
    let mut facts = Vec::new();
    let mut add = |label: &str, value: String| {
        if !value.trim().is_empty() {
            facts.push(TvDetailItem {
                label: label.into(),
                value: value.into(),
            });
        }
    };
    add(
        "Director",
        joined_names(
            credits
                .crew
                .iter()
                .filter(|c| c.job == "Director")
                .map(|c| c.name.as_str()),
        ),
    );
    add(
        "Writers",
        joined_names(
            credits
                .crew
                .iter()
                .filter(|c| matches!(c.job.as_str(), "Writer" | "Screenplay" | "Story"))
                .map(|c| c.name.as_str()),
        ),
    );
    add("Release date", details.release_date.clone());
    add("Status", details.status.clone());
    add(
        "Languages",
        joined_names(details.spoken_languages.iter().map(|l| {
            if l.english_name.is_empty() {
                l.name.as_str()
            } else {
                l.english_name.as_str()
            }
        })),
    );
    add(
        "Studios",
        joined_names(details.production_companies.iter().map(|c| c.name.as_str())),
    );
    add(
        "Countries",
        joined_names(details.production_countries.iter().map(|c| c.name.as_str())),
    );
    if details.original_title != details.title {
        add("Original title", details.original_title.clone());
    }
    if let Some(collection) = &details.belongs_to_collection {
        add("Collection", collection.name.clone());
    }
    if details.budget > 0 {
        add("Budget", dollars(details.budget));
    }
    if details.revenue > 0 {
        add("Box office", dollars(details.revenue));
    }
    if details.vote_count > 0 {
        add("TMDB votes", details.vote_count.to_string());
    }
    facts
}

fn update_details(app: &AppWindow, details: &MovieDetails, credits: &MovieCredits, loading: bool) {
    app.set_movie_tagline(details.tagline.as_str().into());
    app.set_movie_facts(Rc::new(VecModel::from(movie_facts(details, credits))).into());
    let cast = credits
        .cast
        .iter()
        .take(16)
        .map(|person| {
            let photo = load_cached_poster(&person.profile_path);
            MoviePerson {
                name: person.name.as_str().into(),
                role: person.character.as_str().into(),
                initials: make_initials(&person.name),
                loading: loading && !person.profile_path.is_empty() && photo.is_none(),
                photo: photo.unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    app.set_movie_cast(Rc::new(VecModel::from(cast)).into());
}

pub(crate) fn install(app: &AppWindow, services: &Services) {
    let weak = app.as_weak();
    let matched = services.matched_store.clone();
    let cache = services.tmdb_store.clone();
    let api = services.tmdb_api.clone();
    let metadata_api = services.metadata_api.clone();
    let rt = services.rt.clone();
    let generation = Arc::new(AtomicU64::new(0));
    app.on_movie_details_refresh(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let request = generation.fetch_add(1, Ordering::Relaxed) + 1;
        let file_id = app.get_movie_item().file_id.to_string();
        app.set_movie_credits_busy(false);
        app.set_movie_credits_error(false);
        app.set_movie_credits_status("".into());
        update_details(
            &app,
            &MovieDetails::default(),
            &MovieCredits::default(),
            false,
        );
        let Some(movie_id) = matched
            .get_matched_snapshot()
            .ok()
            .and_then(|m| m.movies.get(&file_id).copied())
        else {
            app.set_movie_credits_status("Cast information is unavailable for this movie.".into());
            return;
        };
        let details = cache
            .get_cached_data(&format!("movie_{movie_id}_details_en-US"))
            .and_then(|data| serde_json::from_value::<MovieDetails>(data).ok())
            .unwrap_or_default();
        let cached_credits = cache
            .get_cached_data(&format!("movie_{movie_id}_credits_en-US"))
            .and_then(|data| serde_json::from_value::<MovieCredits>(data).ok());
        update_details(
            &app,
            &details,
            &cached_credits.clone().unwrap_or_default(),
            true,
        );
        app.set_movie_credits_busy(cached_credits.is_none());
        // Optional IMDb rating; the media refresh also updates the open movie item.
        let imdb_weak = weak.clone();
        let metadata_api = metadata_api.clone();
        rt.spawn(async move {
            if metadata_api.refresh_imdb_movie_ratings(&[movie_id]).await {
                let _ = imdb_weak.upgrade_in_event_loop(|app| app.invoke_media_refresh());
            }
        });
        let weak = weak.clone();
        let api = api.clone();
        let generation = generation.clone();
        rt.spawn(async move {
            let result = match cached_credits {
                Some(credits) => Ok(credits),
                None => api.get_movie_credits(movie_id).await,
            };
            let credits = match result {
                Ok(credits) => credits,
                Err(_) => {
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        if generation.load(Ordering::Relaxed) != request
                            || app.get_movie_item().file_id != file_id
                        {
                            return;
                        }
                        app.set_movie_credits_busy(false);
                        app.set_movie_credits_error(true);
                        app.set_movie_credits_status(
                            "Couldn't load cast and crew. Please try again.".into(),
                        );
                    });
                    return;
                }
            };
            let ui_credits = credits.clone();
            let ui_details = details.clone();
            let ui_generation = generation.clone();
            let ui_file = file_id.clone();
            let _ = weak.upgrade_in_event_loop(move |app| {
                if ui_generation.load(Ordering::Relaxed) != request
                    || app.get_movie_item().file_id != ui_file
                {
                    return;
                }
                app.set_movie_credits_busy(false);
                app.set_movie_credits_status(
                    if ui_credits.cast.is_empty() {
                        "No cast information available."
                    } else {
                        ""
                    }
                    .into(),
                );
                update_details(&app, &ui_details, &ui_credits, true);
            });
            // Limit portrait downloads to four concurrent requests.
            for batch in credits.cast.iter().take(16).collect::<Vec<_>>().chunks(4) {
                let mut downloads = tokio::task::JoinSet::new();
                for person in batch {
                    if !person.profile_path.is_empty() {
                        downloads.spawn(download_posters(vec![person.profile_path.clone()]));
                    }
                }
                while downloads.join_next().await.is_some() {}
                if generation.load(Ordering::Relaxed) != request {
                    return;
                }
                let ui_credits = credits.clone();
                let ui_details = details.clone();
                let ui_generation = generation.clone();
                let ui_file = file_id.clone();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if ui_generation.load(Ordering::Relaxed) == request
                        && app.get_movie_item().file_id == ui_file
                    {
                        update_details(&app, &ui_details, &ui_credits, true);
                    }
                });
            }
            let _ = weak.upgrade_in_event_loop(move |app| {
                if generation.load(Ordering::Relaxed) != request
                    || app.get_movie_item().file_id != file_id
                {
                    return;
                }
                update_details(&app, &details, &credits, false);
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_hide_unknown_values_and_deduplicate_writers() {
        let details: MovieDetails = serde_json::from_value(serde_json::json!({
            "title": "Example", "original_title": "Example", "budget": 1234567,
            "tagline": null, "belongs_to_collection": null,
            "spoken_languages": [{"name": "Français", "english_name": "French"}]
        }))
        .unwrap();
        let credits: MovieCredits = serde_json::from_value(serde_json::json!({
            "cast": [{"name": "Actor", "profile_path": null}],
            "crew": [{"name": "Writer", "job": "Story"}, {"name": "Writer", "job": "Screenplay"}]
        }))
        .unwrap();
        let facts = movie_facts(&details, &credits);
        assert!(facts
            .iter()
            .any(|f| f.label == "Budget" && f.value == "US $1,234,567"));
        assert!(facts
            .iter()
            .any(|f| f.label == "Writers" && f.value == "Writer"));
        assert!(facts
            .iter()
            .any(|f| f.label == "Languages" && f.value == "French"));
        assert!(!facts
            .iter()
            .any(|f| matches!(f.label.as_str(), "Box office" | "Original title" | "Status")));
        assert!(credits.cast[0].profile_path.is_empty());
    }
}
