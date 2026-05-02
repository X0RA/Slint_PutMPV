#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use regex::Regex;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::config::ConfigStore;
use crate::storage::tmdb_store::{TMDBCache, TMDBStore};

const BASE_URL: &str = "https://api.themoviedb.org/3";
const LANGUAGE: &str = "en-US";
const IMAGE_LANGUAGES: &str = "en,null";

#[derive(Debug, Clone)]
pub struct TMDBAPI {
    http: Client,
    cfg: Arc<ConfigStore>,
    cache: Arc<TMDBStore>,
}

impl TMDBAPI {
    pub fn new(cfg: Arc<ConfigStore>, cache: Arc<TMDBStore>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("PutMPV/1.0")
            .build()
            .expect("reqwest client build");
        Self { http, cfg, cache }
    }

    pub fn effective_api_key(&self) -> Result<String> {
        let local = self.cfg.tmdb_local_key();
        let putio = self.cfg.tmdb_putio_key();
        let has_local = !local.is_empty();
        let has_putio = !putio.is_empty();
        match (has_local, has_putio, self.cfg.tmdb_source().as_str()) {
            (false, false, _) => Err(anyhow!("no TMDB API key configured")),
            (true, false, _) => Ok(local),
            (false, true, _) => Ok(putio),
            (true, true, "putio") => Ok(putio),
            (true, true, _) => Ok(local),
        }
    }

    pub fn get_cache_snapshot(&self) -> Result<TMDBCache> {
        self.cache.get_cache_snapshot()
    }

    pub async fn search_movie(&self, query: &str, page: i32) -> Result<Vec<MovieSearchResult>> {
        let (clean_query, year) = clean_query_with_year(query);
        let mut params = vec![
            ("query", clean_query),
            ("include_adult", "false".to_string()),
            ("language", LANGUAGE.to_string()),
        ];
        if page > 0 {
            params.push(("page", page.to_string()));
        }
        if let Some(year) = year {
            params.push(("year", year));
        }
        let data = self.request_json("/search/movie", &params).await?;
        let response: MovieSearchResponse = serde_json::from_value(data)?;
        Ok(response.results)
    }

    pub async fn search_tv(&self, query: &str, page: i32) -> Result<Vec<TVSearchResult>> {
        let (clean_query, year) = clean_query_with_year(query);
        let mut params = vec![
            ("query", clean_query),
            ("include_adult", "false".to_string()),
            ("language", LANGUAGE.to_string()),
        ];
        if page > 0 {
            params.push(("page", page.to_string()));
        }
        if let Some(year) = year {
            params.push(("year", year));
        }
        let data = self.request_json("/search/tv", &params).await?;
        let response: TVSearchResponse = serde_json::from_value(data)?;
        Ok(response.results)
    }

    pub async fn get_movie_details(&self, movie_id: i32) -> Result<MovieDetails> {
        let key = format!("movie_{movie_id}_details_{LANGUAGE}");
        self.cached_request(
            &key,
            &format!("/movie/{movie_id}"),
            &[("language", LANGUAGE.to_string())],
        )
        .await
    }

    pub async fn get_tv_series_details(&self, series_id: i32) -> Result<TVSeriesDetails> {
        let key = format!("tv_{series_id}_details_{LANGUAGE}");
        self.cached_request(
            &key,
            &format!("/tv/{series_id}"),
            &[("language", LANGUAGE.to_string())],
        )
        .await
    }

    pub async fn get_tv_season_details(
        &self,
        series_id: i32,
        season_number: i32,
    ) -> Result<TVSeasonDetails> {
        let key = format!("tv_{series_id}_season_{season_number}_{LANGUAGE}");
        let mut season: TVSeasonDetails = self
            .cached_request(
                &key,
                &format!("/tv/{series_id}/season/{season_number}"),
                &[("language", LANGUAGE.to_string())],
            )
            .await?;
        if season.episode_count == 0 && !season.episodes.is_empty() {
            season.episode_count = season.episodes.len() as i32;
        }
        Ok(season)
    }

    pub async fn get_movie_images(&self, movie_id: i32) -> Result<MovieImages> {
        let key = format!("movie_{movie_id}_images_{LANGUAGE}_{IMAGE_LANGUAGES}");
        self.cached_request(
            &key,
            &format!("/movie/{movie_id}/images"),
            &[
                ("language", LANGUAGE.to_string()),
                ("include_image_language", IMAGE_LANGUAGES.to_string()),
            ],
        )
        .await
    }

    pub async fn get_tv_episode_images(
        &self,
        series_id: i32,
        season_number: i32,
        episode_number: i32,
    ) -> Result<TVEpisodeImages> {
        let key = format!(
            "tv_{series_id}_episode_images_{season_number}_{episode_number}_{LANGUAGE}_{IMAGE_LANGUAGES}"
        );
        self.cached_request(
            &key,
            &format!("/tv/{series_id}/season/{season_number}/episode/{episode_number}/images"),
            &[
                ("language", LANGUAGE.to_string()),
                ("include_image_language", IMAGE_LANGUAGES.to_string()),
            ],
        )
        .await
    }

    async fn cached_request<T: serde::de::DeserializeOwned>(
        &self,
        cache_key: &str,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        if let Some(data) = self.cache.get_cached_data(cache_key) {
            if let Ok(parsed) = serde_json::from_value::<T>(data) {
                return Ok(parsed);
            }
        }
        let data = self.request_json(endpoint, params).await?;
        self.cache.set_cached_data(cache_key, data.clone())?;
        Ok(serde_json::from_value(data)?)
    }

    async fn request_json(&self, endpoint: &str, params: &[(&str, String)]) -> Result<Value> {
        let key = self.effective_api_key()?;
        let url = format!("{BASE_URL}{endpoint}");
        let mut query = Vec::with_capacity(params.len() + 1);
        query.push(("api_key", key));
        for (name, value) in params {
            query.push((*name, value.clone()));
        }
        let resp = self.http.get(url).query(&query).send().await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status != StatusCode::OK {
            return Err(anyhow!(
                "TMDB API request failed with status {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        Ok(serde_json::from_slice(&body)?)
    }
}

fn clean_query_with_year(query: &str) -> (String, Option<String>) {
    let year_re = Regex::new(r"(?i)(19[5-9]\d|20[0-4]\d)").unwrap();
    let matches = year_re.find_iter(query).collect::<Vec<_>>();
    let mut clean = query.to_string();
    let year = matches.last().map(|m| {
        let y = m.as_str().to_string();
        clean.replace_range(m.range(), "");
        y
    });
    let whitespace = Regex::new(r"\s+").unwrap();
    (whitespace.replace_all(clean.trim(), " ").to_string(), year)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Genre {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MovieDetails {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub original_title: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub poster_path: String,
    #[serde(default)]
    pub backdrop_path: String,
    #[serde(default)]
    pub runtime: i32,
    #[serde(default)]
    pub genres: Vec<Genre>,
    #[serde(default)]
    pub vote_average: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ImageInfo {
    #[serde(default)]
    pub aspect_ratio: f64,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub iso_639_1: Option<String>,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub vote_average: f64,
    #[serde(default)]
    pub vote_count: i32,
    #[serde(default)]
    pub width: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MovieImages {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub backdrops: Vec<ImageInfo>,
    #[serde(default)]
    pub logos: Vec<ImageInfo>,
    #[serde(default)]
    pub posters: Vec<ImageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Season {
    #[serde(default)]
    pub air_date: String,
    #[serde(default)]
    pub episode_count: i32,
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub poster_path: String,
    #[serde(default)]
    pub season_number: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVSeriesDetails {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub original_name: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub first_air_date: String,
    #[serde(default)]
    pub poster_path: String,
    #[serde(default)]
    pub backdrop_path: String,
    #[serde(default)]
    pub number_of_episodes: i32,
    #[serde(default)]
    pub number_of_seasons: i32,
    #[serde(default)]
    pub vote_average: f64,
    #[serde(default)]
    pub seasons: Vec<Season>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Episode {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub air_date: String,
    #[serde(default)]
    pub episode_number: i32,
    #[serde(default)]
    pub season_number: i32,
    #[serde(default)]
    pub still_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVSeasonDetails {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub air_date: String,
    #[serde(default)]
    pub episodes: Vec<Episode>,
    #[serde(default)]
    pub episode_count: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub season_number: i32,
    #[serde(default)]
    pub poster_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVEpisodeImages {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub stills: Vec<ImageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MovieSearchResult {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub original_title: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub poster_path: String,
    #[serde(default)]
    pub backdrop_path: String,
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub vote_average: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct MovieSearchResponse {
    #[serde(default)]
    page: i32,
    #[serde(default)]
    results: Vec<MovieSearchResult>,
    #[serde(default)]
    total_pages: i32,
    #[serde(default)]
    total_results: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVSearchResult {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub original_name: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub poster_path: String,
    #[serde(default)]
    pub backdrop_path: String,
    #[serde(default)]
    pub first_air_date: String,
    #[serde(default)]
    pub vote_average: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct TVSearchResponse {
    #[serde(default)]
    page: i32,
    #[serde(default)]
    results: Vec<TVSearchResult>,
    #[serde(default)]
    total_pages: i32,
    #[serde(default)]
    total_results: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_year_from_query() {
        let (query, year) = clean_query_with_year("Alien 1979");
        assert_eq!(query, "Alien");
        assert_eq!(year.as_deref(), Some("1979"));
    }
}
