use super::*;
use serde_json::json;
fn episode() -> EpisodeContext {
    EpisodeContext {
        source: MetadataSource::Tmdb,
        series_id: 62110,
        episode_id: 1,
        season: 1,
        episode: 1,
        title: "Assassination Time".into(),
        anime: true,
        ids: ExternalIds {
            tvdb_id: Some(283947),
            imdb_id: Some("tt3837246".into()),
        },
        file_version: "size:modified".into(),
    }
}
#[test]
fn runtime_and_identity_validation_for_all_providers() {
    let ep = episode();
    let mut data = json!({"type":"tv","tvdb_id":283947,"season":1,"episode":1,"group_duration_ms":1440000,
        "intro":[{"start_ms":30000,"end_ms":120000,"confidence":0.7}],"credits":[{"start_ms":1380000,"end_ms":null}]});
    assert_eq!(
        parse_theintrodb(&data, &ep, &ep.ids, 1440.0).unwrap().len(),
        2
    );
    assert!(parse_theintrodb(&data, &ep, &ep.ids, 1500.0)
        .unwrap()
        .is_empty());
    data["episode"] = json!(2);
    assert!(parse_theintrodb(&data, &ep, &ep.ids, 1440.0).is_err());
    data["episode"] = json!(1);
    data["tvdb_id"] = json!(1);
    assert!(parse_theintrodb(&data, &ep, &ep.ids, 1440.0).is_err());
    let fallback = json!({"imdb_id":"tt3837246","season":1,"episode":1,"intro":{"start_ms":30000,"end_ms":120000,"confidence":1.0}});
    assert_eq!(
        parse_introdb(&fallback, &ep, "tt3837246", 1440.0)
            .unwrap()
            .len(),
        1
    );
    assert!(parse_introdb(&fallback, &ep, "tt9999999", 1440.0).is_err());
    let anime = json!({"found":true,"results":[{"skipType":"op","episodeLength":1395.98,"interval":{"startTime":211.499,"endTime":301.499}}]});
    assert_eq!(parse_aniskip(&anime, 1396.0).unwrap().len(), 1);
    assert!(parse_aniskip(&anime, 1440.0).unwrap().is_empty());
}
#[test]
fn anime_mapping_distinguishes_seasons_offsets_and_ambiguity() {
    let entries:Vec<AnimeEntry>=serde_json::from_value(json!([
        {"mal_id":24833,"themoviedb_id":{"tv":62110},"season":{"tmdb":1}},
        {"mal_id":30654,"themoviedb_id":{"tv":62110},"season":{"tmdb":2},"episode_offset":{"tmdb":12}}
    ])).unwrap();
    let mut ep = episode();
    assert_eq!(
        map_anime(&entries, &ep, &ep.ids),
        Some(AnimeMapping {
            mal_id: 24833,
            episode: 1
        })
    );
    ep.season = 2;
    ep.episode = 13;
    assert_eq!(
        map_anime(&entries, &ep, &ep.ids),
        Some(AnimeMapping {
            mal_id: 30654,
            episode: 1
        })
    );
    ep.episode = 1;
    assert!(map_anime(&entries, &ep, &ep.ids).is_none());
    ep.season = 1;
    let mut ambiguous = entries.clone();
    ambiguous.push(entries[0].clone());
    assert!(map_anime(&ambiguous, &ep, &ep.ids).is_none());
}
#[test]
fn cache_separates_files_releases_and_scraped_identities() {
    let mut ep = episode();
    let key = cache_key(1, &ep, 1440.0);
    ep.ids = ExternalIds::default();
    assert_eq!(key, cache_key(1, &ep, 1440.0));
    assert_ne!(key, cache_key(2, &ep, 1440.0));
    assert_ne!(key, cache_key(1, &ep, 1450.0));
    ep.episode = 2;
    assert_ne!(key, cache_key(1, &ep, 1440.0));
    ep.episode = 1;
    ep.file_version = "new".into();
    assert_ne!(key, cache_key(1, &ep, 1440.0));
    assert_eq!(cache_ttl(true, false), POSITIVE_TTL);
    assert_eq!(cache_ttl(false, false), NEGATIVE_TTL);
    assert_eq!(cache_ttl(false, true), ERROR_TTL);
    assert_eq!(cache_ttl(true, true), ERROR_TTL);
}
