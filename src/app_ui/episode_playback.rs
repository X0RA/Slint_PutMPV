//! Build eligible Media queues from existing matches and cached metadata only.
use std::collections::{BTreeMap, HashMap};

use crate::metadata::tmdb::{Episode, TVSeasonDetails, TVSeriesDetails};
use crate::player::PlaybackQueueItem;
use crate::putio::types::{DirectoryNode, PutIoFile, UnifiedDirectoryTree};
use crate::segments::{EpisodeContext, ExternalIds, MetadataSource};
use crate::storage::{
    matched_store::MatchedData, tmdb_store::TMDBCache, tvmaze_store::TVMazeCache,
};

pub fn build_queue(
    tree: &UnifiedDirectoryTree,
    mut matched: MatchedData,
    mut tmdb: TMDBCache,
    maze: &TVMazeCache,
    series_id: i32,
    selected_id: u64,
) -> Option<Vec<PlaybackQueueItem>> {
    super::media_library::merge_tvmaze(&mut tmdb, &mut matched, maze);
    let sub = tmdb.tv.get(&series_id.to_string())?;
    let details: TVSeriesDetails = sub
        .iter()
        .filter(|(key, _)| key.starts_with("details_"))
        .find_map(|(_, entry)| serde_json::from_value(entry.data.clone()).ok())?;
    let mut episodes = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for (key, entry) in sub {
        if !key.starts_with("season_") {
            continue;
        }
        let Ok(season) = serde_json::from_value::<TVSeasonDetails>(entry.data.clone()) else {
            continue;
        };
        let expected = details
            .seasons
            .iter()
            .find(|s| s.season_number == season.season_number)
            .map(|s| s.episode_count)
            .filter(|c| *c > 0)
            .unwrap_or(season.episode_count);
        counts.insert(season.season_number, expected);
        for ep in season.episodes {
            if ep.id != 0 && ep.episode_number > 0 {
                episodes.insert((ep.season_number, ep.episode_number), ep);
            }
        }
    }
    let mut files = HashMap::new();
    collect_files(&tree.root, &mut files);
    let selected_episode = matched.tv.get(&selected_id.to_string())?;
    let current = episodes
        .iter()
        .find(|(_, ep)| ep.id == *selected_episode)
        .map(|(key, _)| *key)?;
    let source = if series_id > 0 {
        MetadataSource::Tmdb
    } else {
        MetadataSource::Tvmaze
    };
    let external = if source == MetadataSource::Tmdb {
        sub.get("external_ids").map(|e| &e.data)
    } else {
        maze.data
            .get(&format!("show_{}_details", series_id.abs()))
            .and_then(|e| e.data.get("externals"))
    };
    let ids = ExternalIds {
        tvdb_id: external
            .and_then(|v| v.get("tvdb_id").or_else(|| v.get("thetvdb")))
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| *v > 0),
        imdb_id: external
            .and_then(|v| v.get("imdb_id").or_else(|| v.get("imdb")))
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    let anime = details
        .genres
        .iter()
        .any(|g| g.id == 16 || g.name == "Animation" || g.name == "Anime");
    let mut by_episode: HashMap<i32, Vec<&PutIoFile>> = HashMap::new();
    for (file_id, episode_id) in &matched.tv {
        if let Some(file) = file_id.parse::<u64>().ok().and_then(|id| files.get(&id)) {
            by_episode.entry(*episode_id).or_default().push(file);
        }
    }
    for candidates in by_episode.values_mut() {
        candidates.sort_by_key(|f| f.id);
    }
    let mut queue = Vec::new();
    let mut key = current;
    while let Some(ep) = episodes.get(&key) {
        let file = if key == current {
            files.get(&selected_id).copied()
        } else {
            by_episode.get(&ep.id).and_then(|v| v.first().copied())
        };
        let Some(file) = file else {
            break;
        }; // Never silently jump over a missing episode.
        let mut meta = format!("S{:02} E{:02}", key.0, key.1);
        if ep.runtime > 0 {
            meta.push_str(&format!(" · {}m", ep.runtime));
        }
        queue.push(PlaybackQueueItem {
            file_id: file.id,
            title: ep.name.clone(),
            meta,
            episode: Some(EpisodeContext {
                source,
                series_id: series_id.abs(),
                episode_id: ep.id.abs(),
                season: key.0,
                episode: key.1,
                title: ep.name.clone(),
                anime,
                ids: ids.clone(),
                file_version: format!(
                    "{}:{}",
                    file.size,
                    file.updated_at.as_deref().unwrap_or_default()
                ),
            }),
        });
        let Some(next) = next_key(key, &episodes, &counts) else {
            break;
        };
        key = next;
    }
    (!queue.is_empty()).then_some(queue)
}

fn next_key(
    key: (i32, i32),
    episodes: &BTreeMap<(i32, i32), Episode>,
    counts: &BTreeMap<i32, i32>,
) -> Option<(i32, i32)> {
    if key.0 == 0 {
        return None;
    } // Specials require an explicit selection.
    let within = (key.0, key.1 + 1);
    if episodes.contains_key(&within) {
        return Some(within);
    }
    let expected = *counts.get(&key.0)?;
    if expected != key.1 || !(1..=expected).all(|ep| episodes.contains_key(&(key.0, ep))) {
        return None;
    }
    let next = (key.0 + 1, 1);
    episodes.contains_key(&next).then_some(next)
}

fn collect_files<'a>(node: &'a DirectoryNode, out: &mut HashMap<u64, &'a PutIoFile>) {
    for file in &node.files {
        out.insert(file.id, file);
    }
    for child in &node.children {
        collect_files(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> (UnifiedDirectoryTree, MatchedData, TMDBCache) {
        use crate::storage::tmdb_store::CacheEntry;
        use serde_json::json;
        let mut cache = TMDBCache::default();
        let sub = cache.tv.entry("42".into()).or_default();
        for (key, data) in [
            (
                "details_en-US",
                json!({"id":42,"name":"Show","seasons":[{"season_number":1,"episode_count":2},{"season_number":2,"episode_count":1}]}),
            ),
            (
                "season_1_en-US",
                json!({"season_number":1,"episode_count":2,"episodes":[
                {"id":11,"name":"One","season_number":1,"episode_number":1},
                {"id":12,"name":"Two","season_number":1,"episode_number":2}]}),
            ),
            (
                "season_2_en-US",
                json!({"season_number":2,"episode_count":1,"episodes":[
                {"id":21,"name":"Three","season_number":2,"episode_number":1}]}),
            ),
        ] {
            sub.insert(key.into(), CacheEntry { data });
        }
        let tree = UnifiedDirectoryTree {
            root: DirectoryNode {
                files: [100, 101, 102, 200]
                    .into_iter()
                    .map(|id| PutIoFile {
                        id,
                        size: 1000,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        let matches = MatchedData {
            tv: HashMap::from([
                ("100".into(), 11),
                ("101".into(), 12),
                ("102".into(), 12),
                ("200".into(), 21),
            ]),
            ..Default::default()
        };
        (tree, matches, cache)
    }
    #[test]
    fn queues_require_scraped_matches_and_preserve_selected_release() {
        let (mut tree, mut matches, cache) = fixtures();
        let maze = TVMazeCache::default();
        let queue = build_queue(&tree, matches.clone(), cache.clone(), &maze, 42, 100).unwrap();
        assert_eq!(
            queue.iter().map(|q| q.file_id).collect::<Vec<_>>(),
            vec![100, 101, 200]
        );
        assert!(queue.iter().all(|q| q.episode.is_some()));
        let queue = build_queue(&tree, matches.clone(), cache.clone(), &maze, 42, 102).unwrap();
        assert_eq!(queue[0].file_id, 102);
        matches.tv.remove("100");
        assert!(build_queue(&tree, matches.clone(), cache.clone(), &maze, 42, 100).is_none());
        matches.tv.insert("100".into(), 11);
        tree.root.files.retain(|f| f.id != 101 && f.id != 102);
        let queue = build_queue(&tree, matches.clone(), cache.clone(), &maze, 42, 100).unwrap();
        assert_eq!(queue.len(), 1); // Don't jump from S01E01 to S02E01.
        matches.tv_source.insert("100".into(), "tvmaze".into());
        assert!(build_queue(&tree, matches, cache, &maze, 42, 100).is_none());
    }
    #[test]
    fn progression_requires_consecutive_metadata_and_complete_season() {
        let mut eps = BTreeMap::new();
        for key in [(1, 1), (1, 2), (1, 4), (2, 1), (0, 1)] {
            eps.insert(key, Episode::default());
        }
        let counts = BTreeMap::from([(1, 4), (2, 1)]);
        assert_eq!(next_key((1, 1), &eps, &counts), Some((1, 2)));
        assert_eq!(next_key((1, 2), &eps, &counts), None);
        assert_eq!(next_key((1, 4), &eps, &counts), None);
        eps.insert((1, 3), Episode::default());
        assert_eq!(next_key((1, 4), &eps, &counts), Some((2, 1)));
        assert_eq!(next_key((0, 1), &eps, &counts), None);
    }
}
