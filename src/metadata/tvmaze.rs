#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::metadata::api::{AbsoluteEpisodeResult, EpisodeRefByFileID};
use crate::storage::tvmaze_store::{TVMazeCache, TVMazeStore};

const BASE_URL: &str = "https://api.tvmaze.com";

#[derive(Debug, Clone)]
pub struct TVMazeAPI {
    http: Client,
    cache: Arc<TVMazeStore>,
}

impl TVMazeAPI {
    pub fn new(cache: Arc<TVMazeStore>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("PutMPV/1.0")
            .build()
            .expect("reqwest client build");
        Self { http, cache }
    }

    pub fn get_cache_snapshot(&self) -> Result<TVMazeCache> {
        self.cache.get_cache_snapshot()
    }

    pub async fn search_shows(&self, query: &str) -> Result<Vec<TVMazeSearchResult>> {
        let data = self
            .request_json("/search/shows", &[("q", query.to_string())])
            .await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn get_show_details(&self, show_id: i32) -> Result<TVMazeShow> {
        let key = format!("show_{show_id}_details");
        self.cached_request(&key, &format!("/shows/{show_id}"), &[])
            .await
    }

    pub async fn get_show_seasons(&self, show_id: i32) -> Result<Vec<TVMazeSeason>> {
        let key = format!("show_{show_id}_seasons");
        self.cached_request(&key, &format!("/shows/{show_id}/seasons"), &[])
            .await
    }

    pub async fn get_season_episodes(&self, season_id: i32) -> Result<Vec<TVMazeEpisode>> {
        let key = format!("season_{season_id}_episodes");
        self.cached_request(&key, &format!("/seasons/{season_id}/episodes"), &[])
            .await
    }

    pub async fn resolve_tv_episodes_by_file_id(
        &self,
        show_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<HashMap<String, i32>> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let seasons = self.get_show_seasons(show_id).await?;
        let season_num_to_id = seasons
            .into_iter()
            .map(|s| (s.number, s.id))
            .collect::<HashMap<_, _>>();
        let needed = items
            .iter()
            .filter_map(|it| (it.season > 0).then_some(it.season))
            .collect::<BTreeSet<_>>();
        let mut season_lookup = HashMap::<i32, HashMap<i32, i32>>::new();
        for season_num in needed {
            let Some(season_id) = season_num_to_id.get(&season_num).copied() else {
                continue;
            };
            let episodes = self.get_season_episodes(season_id).await?;
            season_lookup.insert(season_num, numbered_episode_lookup(&episodes));
        }
        let mut result = HashMap::new();
        for item in items {
            if item.file_id.is_empty() || item.season <= 0 || item.episode <= 0 {
                continue;
            }
            if let Some(id) = season_lookup
                .get(&item.season)
                .and_then(|eps| eps.get(&item.episode))
            {
                result.insert(item.file_id.clone(), *id);
            }
        }
        Ok(result)
    }

    pub async fn resolve_absolute_episodes(
        &self,
        show_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<AbsoluteEpisodeResult> {
        if items.is_empty() {
            return Ok(AbsoluteEpisodeResult::default());
        }
        let seasons = self.get_show_seasons(show_id).await?;
        let mut regular = seasons
            .into_iter()
            .filter(|s| s.number > 0)
            .collect::<Vec<_>>();
        regular.sort_by_key(|s| s.number);

        let mut ranges = Vec::new();
        let mut cumulative = 0;
        for season in regular {
            let episodes = self.get_season_episodes(season.id).await?;
            let numbered = episodes
                .into_iter()
                .filter(|ep| ep.number > 0)
                .collect::<Vec<_>>();
            if numbered.is_empty() {
                continue;
            }
            let start = cumulative + 1;
            let end = cumulative + numbered.len() as i32;
            cumulative = end;
            ranges.push(AbsoluteRange {
                season_number: season.number,
                start_abs: start,
                end_abs: end,
                episodes: numbered,
            });
        }

        let mut resolved = HashMap::new();
        let mut used = BTreeSet::new();
        for item in items {
            if item.file_id.is_empty() || item.episode <= 0 {
                continue;
            }
            let Some(range) = ranges
                .iter()
                .find(|r| item.episode >= r.start_abs && item.episode <= r.end_abs)
            else {
                continue;
            };
            let relative_ep = item.episode - range.start_abs + 1;
            if let Some(ep) = range.episodes.iter().find(|ep| ep.number == relative_ep) {
                resolved.insert(item.file_id.clone(), ep.id);
                used.insert(range.season_number);
            }
        }
        Ok(AbsoluteEpisodeResult {
            resolved,
            seasons: used.into_iter().collect(),
        })
    }

    pub async fn seed_tv(&self, show_id: i32, season_numbers: &[i32]) -> Result<usize> {
        let _ = self.get_show_details(show_id).await?;
        let seasons = self.get_show_seasons(show_id).await?;
        let season_num_to_id = seasons
            .into_iter()
            .map(|s| (s.number, s.id))
            .collect::<HashMap<_, _>>();
        let uniq = season_numbers
            .iter()
            .copied()
            .filter(|s| *s > 0)
            .collect::<BTreeSet<_>>();
        let mut ok = 0;
        for season_num in uniq {
            let Some(season_id) = season_num_to_id.get(&season_num).copied() else {
                continue;
            };
            if self.get_season_episodes(season_id).await.is_ok() {
                ok += 1;
            }
        }
        Ok(ok)
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
        let url = format!("{BASE_URL}{endpoint}");
        let resp = self.http.get(url).query(params).send().await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if status != StatusCode::OK {
            return Err(anyhow!(
                "TVMaze API request failed with status {status}: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        Ok(serde_json::from_slice(&body)?)
    }
}

fn numbered_episode_lookup(episodes: &[TVMazeEpisode]) -> HashMap<i32, i32> {
    episodes
        .iter()
        .filter(|ep| ep.number > 0)
        .map(|ep| (ep.number, ep.id))
        .collect()
}

#[derive(Debug, Clone)]
struct AbsoluteRange {
    season_number: i32,
    start_abs: i32,
    end_abs: i32,
    episodes: Vec<TVMazeEpisode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeSearchResult {
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub show: TVMazeShow,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeShow {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub runtime: Option<i32>,
    #[serde(default)]
    pub premiered: String,
    #[serde(default)]
    pub ended: String,
    #[serde(default)]
    pub image: Option<TVMazeImage>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub externals: TVMazeExternals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeImage {
    #[serde(default)]
    pub medium: String,
    #[serde(default)]
    pub original: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeExternals {
    #[serde(default)]
    pub tvrage: Option<i32>,
    #[serde(default)]
    pub thetvdb: Option<i32>,
    #[serde(default)]
    pub imdb: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeSeason {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub number: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "episodeOrder")]
    pub episode_order: Option<i32>,
    #[serde(default, rename = "premiereDate")]
    pub premiere_date: String,
    #[serde(default, rename = "endDate")]
    pub end_date: String,
    #[serde(default)]
    pub image: Option<TVMazeImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TVMazeEpisode {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub season: i32,
    #[serde(default)]
    pub number: i32,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub airdate: String,
    #[serde(default)]
    pub airtime: String,
    #[serde(default)]
    pub runtime: Option<i32>,
    #[serde(default)]
    pub image: Option<TVMazeImage>,
    #[serde(default)]
    pub summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbered_lookup_skips_unnumbered_episodes() {
        let lookup = numbered_episode_lookup(&[
            TVMazeEpisode {
                id: 1,
                number: 1,
                ..TVMazeEpisode::default()
            },
            TVMazeEpisode {
                id: 2,
                number: 0,
                ..TVMazeEpisode::default()
            },
        ]);
        assert_eq!(lookup.len(), 1);
        assert_eq!(lookup[&1], 1);
    }

    #[tokio::test]
    async fn absolute_resolver_skips_unnumbered_specials() {
        let api = cached_test_api("tvmaze_abs_specials");
        api.cache
            .set_cached_data(
                "show_495_seasons",
                json!([
                    {"id": 176471, "number": 2002, "name": "#1 - #13"},
                    {"id": 1948, "number": 2003, "name": "#14 - #64"},
                    {"id": 1949, "number": 2004, "name": "#65 - #115"},
                    {"id": 1950, "number": 2005, "name": "#116 - #165"},
                    {"id": 1951, "number": 2006, "name": "#166 - #215"}
                ]),
            )
            .unwrap();
        seed_episodes(&api, 176471, 2002, 13, 45006, &[]);
        seed_episodes(
            &api,
            1948,
            2003,
            51,
            45019,
            &[
                (45226, "Naruto: Find the Four-Leaf Red Clover!"),
                (45227, "Naruto: Mission: Protect the Waterfall Village!"),
            ],
        );
        seed_episodes(
            &api,
            1949,
            2004,
            51,
            45070,
            &[(45230, "Naruto: Hidden Leaf Village Grand Sports Festival")],
        );
        seed_episodes(
            &api,
            1950,
            2005,
            50,
            45121,
            &[(
                45231,
                "Finally a Clash! Jounin vs. Genin! All-out Major League Tournament Brawl Begins!!",
            )],
        );
        seed_episodes(&api, 1951, 2006, 50, 45171, &[]);

        let result = api
            .resolve_absolute_episodes(
                495,
                &[
                    EpisodeRefByFileID {
                        file_id: "165".to_string(),
                        episode: 165,
                        ..EpisodeRefByFileID::default()
                    },
                    EpisodeRefByFileID {
                        file_id: "166".to_string(),
                        episode: 166,
                        ..EpisodeRefByFileID::default()
                    },
                    EpisodeRefByFileID {
                        file_id: "169".to_string(),
                        episode: 169,
                        ..EpisodeRefByFileID::default()
                    },
                    EpisodeRefByFileID {
                        file_id: "170".to_string(),
                        episode: 170,
                        ..EpisodeRefByFileID::default()
                    },
                ],
            )
            .await
            .unwrap();

        assert_eq!(result.resolved["165"], 45170);
        assert_eq!(result.resolved["166"], 45171);
        assert_eq!(result.resolved["169"], 45174);
        assert_eq!(result.resolved["170"], 45175);
        assert_eq!(result.seasons, vec![2005, 2006]);
    }

    #[tokio::test]
    async fn absolute_resolver_ignores_multiple_embedded_specials() {
        let api = cached_test_api("tvmaze_abs_embedded_specials");
        api.cache
            .set_cached_data(
                "show_999_seasons",
                json!([
                    {"id": 1001, "number": 1, "name": "Season 1"},
                    {"id": 1002, "number": 2, "name": "Season 2"}
                ]),
            )
            .unwrap();
        seed_episodes(
            &api,
            1001,
            1,
            3,
            11,
            &[(21, "Special A"), (22, "Special B")],
        );
        seed_episodes(&api, 1002, 2, 2, 31, &[]);

        let result = api
            .resolve_absolute_episodes(
                999,
                &[
                    EpisodeRefByFileID {
                        file_id: "3".to_string(),
                        episode: 3,
                        ..EpisodeRefByFileID::default()
                    },
                    EpisodeRefByFileID {
                        file_id: "4".to_string(),
                        episode: 4,
                        ..EpisodeRefByFileID::default()
                    },
                    EpisodeRefByFileID {
                        file_id: "5".to_string(),
                        episode: 5,
                        ..EpisodeRefByFileID::default()
                    },
                ],
            )
            .await
            .unwrap();

        assert_eq!(result.resolved["3"], 13);
        assert_eq!(result.resolved["4"], 31);
        assert_eq!(result.resolved["5"], 32);
        assert_eq!(result.seasons, vec![1, 2]);
    }

    fn cached_test_api(name: &str) -> TVMazeAPI {
        let path = std::env::temp_dir().join(format!("putmpv_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &path);
        TVMazeAPI::new(Arc::new(TVMazeStore::load().unwrap()))
    }

    fn seed_episodes(
        api: &TVMazeAPI,
        season_id: i32,
        season_number: i32,
        count: i32,
        base_id: i32,
        specials: &[(i32, &str)],
    ) {
        let mut episodes = (1..=count)
            .map(|number| {
                json!({
                    "id": base_id + number - 1,
                    "name": format!("Episode {number}"),
                    "season": season_number,
                    "number": number,
                    "type": "regular"
                })
            })
            .collect::<Vec<_>>();
        episodes.extend(specials.iter().map(|(id, name)| {
            json!({
                "id": id,
                "name": name,
                "season": season_number,
                "number": 0,
                "type": "significant_special"
            })
        }));
        api.cache
            .set_cached_data(&format!("season_{season_id}_episodes"), json!(episodes))
            .unwrap();
    }
}
