#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::metadata::tmdb::{Season, TMDBAPI};
use crate::metadata::tvmaze::TVMazeAPI;
use crate::storage::matched_store::{MatchedData, MatchedStore};

#[derive(Debug, Clone)]
pub struct MetadataAPI {
    matched: Arc<MatchedStore>,
    tmdb: Arc<TMDBAPI>,
    tvmaze: Arc<TVMazeAPI>,
}

impl MetadataAPI {
    pub fn new(matched: Arc<MatchedStore>, tmdb: Arc<TMDBAPI>, tvmaze: Arc<TVMazeAPI>) -> Self {
        Self {
            matched,
            tmdb,
            tvmaze,
        }
    }

    pub async fn seed_movies(&self, movie_ids: &[i32]) -> Result<usize> {
        let mut ok = 0;
        for id in movie_ids.iter().copied().filter(|id| *id > 0) {
            if self.tmdb.get_movie_details(id).await.is_ok() {
                ok += 1;
            }
        }
        Ok(ok)
    }

    pub async fn seed_tv(&self, series_id: i32, seasons: &[i32]) -> Result<usize> {
        if series_id <= 0 {
            return Err(anyhow!("invalid series id"));
        }
        let _ = self.tmdb.get_tv_series_details(series_id).await?;
        let uniq = seasons
            .iter()
            .copied()
            .filter(|s| *s > 0)
            .collect::<BTreeSet<_>>();
        let mut ok = 0;
        for season in uniq {
            if self
                .tmdb
                .get_tv_season_details(series_id, season)
                .await
                .is_ok()
            {
                ok += 1;
            }
        }
        Ok(ok)
    }

    pub async fn seed_tvmaze(&self, show_id: i32, seasons: &[i32]) -> Result<usize> {
        self.tvmaze.seed_tv(show_id, seasons).await
    }

    pub async fn resolve_tv_episodes_by_file_id(
        &self,
        series_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<HashMap<String, i32>> {
        if series_id <= 0 {
            return Err(anyhow!("invalid series id"));
        }
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let needed = items
            .iter()
            .filter_map(|it| (it.season > 0).then_some(it.season))
            .collect::<BTreeSet<_>>();
        let mut season_lookup = HashMap::<i32, HashMap<i32, i32>>::new();
        for season in needed {
            let details = match self.tmdb.get_tv_season_details(series_id, season).await {
                Ok(details) => details,
                Err(_) => continue,
            };
            season_lookup.insert(
                season,
                details
                    .episodes
                    .into_iter()
                    .map(|ep| (ep.episode_number, ep.id))
                    .collect(),
            );
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

    pub async fn resolve_tvmaze_episodes_by_file_id(
        &self,
        show_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<HashMap<String, i32>> {
        self.tvmaze
            .resolve_tv_episodes_by_file_id(show_id, items)
            .await
    }

    pub async fn resolve_absolute_episodes(
        &self,
        series_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<AbsoluteEpisodeResult> {
        if series_id <= 0 {
            return Err(anyhow!("invalid series id"));
        }
        if items.is_empty() {
            return Ok(AbsoluteEpisodeResult::default());
        }
        let series = self.tmdb.get_tv_series_details(series_id).await?;
        let mut seasons = series
            .seasons
            .into_iter()
            .filter(|s| s.season_number > 0 && s.episode_count > 0)
            .collect::<Vec<_>>();
        seasons.sort_by_key(|s| s.season_number);

        let ranges = build_tmdb_absolute_ranges(&seasons);
        let mut pending = Vec::new();
        let mut needed = BTreeSet::new();
        for item in items {
            if item.file_id.is_empty() || item.episode <= 0 {
                continue;
            }
            let Some((season, relative_ep)) =
                map_absolute_episode_to_tmdb_season(&ranges, item.episode)
            else {
                continue;
            };
            pending.push((item.file_id.clone(), season, item.episode, relative_ep));
            needed.insert(season);
        }
        let mut lookup = HashMap::<i32, HashMap<i32, i32>>::new();
        for season in &needed {
            let details = match self.tmdb.get_tv_season_details(series_id, *season).await {
                Ok(details) => details,
                Err(_) => continue,
            };
            lookup.insert(
                *season,
                details
                    .episodes
                    .into_iter()
                    .map(|ep| (ep.episode_number, ep.id))
                    .collect(),
            );
        }
        let mut resolved = HashMap::new();
        for (file_id, season, abs_ep, rel_ep) in pending {
            if let Some(eps) = lookup.get(&season) {
                if let Some(id) = lookup_absolute_or_relative_episode(eps, abs_ep, rel_ep) {
                    resolved.insert(file_id, id);
                }
            }
        }
        Ok(AbsoluteEpisodeResult {
            resolved,
            seasons: needed.into_iter().collect(),
        })
    }

    pub async fn resolve_tvmaze_absolute_episodes(
        &self,
        show_id: i32,
        items: &[EpisodeRefByFileID],
    ) -> Result<AbsoluteEpisodeResult> {
        self.tvmaze.resolve_absolute_episodes(show_id, items).await
    }

    pub fn bulk_store_matches_by_file_id(&self, matches: &[MatchItemByFileID]) -> Result<()> {
        for item in matches {
            let source = if item.source.is_empty() {
                "tmdb"
            } else {
                item.source.as_str()
            };
            match item.kind.as_str() {
                "movie" => self.matched.insert_movie(&item.file_id, item.tmdb_id)?,
                "episode" => {
                    self.matched
                        .insert_tv_with_source(&item.file_id, item.tmdb_id, source)?;
                }
                _ => return Err(anyhow!("unknown match kind: {}", item.kind)),
            }
        }
        Ok(())
    }

    pub fn get_all_matches(&self) -> Result<MatchedData> {
        self.matched.get_matched_snapshot()
    }

    pub fn delete_match(&self, file_id: &str) -> Result<()> {
        self.matched.delete_movie(file_id)?;
        self.matched.delete_tv(file_id)?;
        Ok(())
    }

    pub fn clear_all_matches(&self) -> Result<()> {
        self.matched.clear()
    }
}

fn build_tmdb_absolute_ranges(seasons: &[Season]) -> Vec<TMDBAbsoluteRange> {
    let mut cumulative = 0;
    seasons
        .iter()
        .map(|season| {
            let start = cumulative + 1;
            let end = cumulative + season.episode_count;
            cumulative = end;
            TMDBAbsoluteRange {
                season_number: season.season_number,
                start_abs: start,
                end_abs: end,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct TMDBAbsoluteRange {
    season_number: i32,
    start_abs: i32,
    end_abs: i32,
}

fn map_absolute_episode_to_tmdb_season(
    ranges: &[TMDBAbsoluteRange],
    episode: i32,
) -> Option<(i32, i32)> {
    let range = ranges
        .iter()
        .find(|r| episode >= r.start_abs && episode <= r.end_abs)?;
    Some((range.season_number, episode - range.start_abs + 1))
}

fn lookup_absolute_or_relative_episode(
    episodes: &HashMap<i32, i32>,
    absolute_episode: i32,
    relative_episode: i32,
) -> Option<i32> {
    episodes
        .get(&absolute_episode)
        .or_else(|| episodes.get(&relative_episode))
        .copied()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EpisodeRefByFileID {
    pub file_id: String,
    pub season: i32,
    pub episode: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MatchItemByFileID {
    pub file_id: String,
    pub kind: String,
    pub tmdb_id: i32,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AbsoluteEpisodeResult {
    #[serde(default)]
    pub resolved: HashMap<String, i32>,
    #[serde(default)]
    pub seasons: Vec<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmdb_absolute_ranges_are_cumulative() {
        let ranges = build_tmdb_absolute_ranges(&[
            Season {
                season_number: 1,
                episode_count: 2,
                ..Season::default()
            },
            Season {
                season_number: 2,
                episode_count: 3,
                ..Season::default()
            },
        ]);
        assert_eq!(ranges[0].start_abs, 1);
        assert_eq!(ranges[0].end_abs, 2);
        assert_eq!(ranges[1].start_abs, 3);
        assert_eq!(ranges[1].end_abs, 5);
    }

    #[test]
    fn tmdb_absolute_episode_maps_to_relative_season_episode() {
        let ranges = build_tmdb_absolute_ranges(&[
            Season {
                season_number: 1,
                episode_count: 50,
                ..Season::default()
            },
            Season {
                season_number: 2,
                episode_count: 56,
                ..Season::default()
            },
            Season {
                season_number: 3,
                episode_count: 54,
                ..Season::default()
            },
        ]);

        assert_eq!(
            map_absolute_episode_to_tmdb_season(&ranges, 120),
            Some((3, 14))
        );
    }

    #[test]
    fn tmdb_absolute_lookup_prefers_absolute_episode_number() {
        let episodes = HashMap::from([(1, 1001), (142, 3142)]);

        assert_eq!(
            lookup_absolute_or_relative_episode(&episodes, 142, 1),
            Some(3142)
        );
    }
}
