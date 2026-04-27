use std::collections::BTreeMap;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::putio::types::{DirectoryNode, PutIoFile};

const MIN_VIDEO_SIZE_BYTES: u64 = 50 * 1024 * 1024;

static EXT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\.[a-z0-9]{2,5}$").unwrap());
static SUPPLEMENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)[.\-_ ](trailer|sample)\.[a-z0-9]+$").unwrap());
static SXX_EXX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\bS(\d{1,2})[ ._\-]*E(\d{1,3})(?:[ ._\-]*(?:E|-)(\d{1,3}))?\b").unwrap()
});
static X_EP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\b(\d{1,2})x(\d{2,3})\b").unwrap());
static DASH_EP_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:^|[ ._\]])-\s*(\d{1,4})(?:\s*-|\s+)(.*)$").unwrap());

static SEASON_FOLDER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(?:season|series)[ ._\-]*(\d{1,2})\b").unwrap());
static SHORT_SEASON_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\bS(\d{1,2})\b").unwrap());
static COMPLETE_RANGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\((\d{1,4})\s*-\s*(\d{1,4})\s*\+\s*Movies?\)").unwrap());
static EXCESS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[[^\]]+\]|\([^\)]+\)").unwrap());
static YEAR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(19\d{2}|20[0-2]\d)\b").unwrap());
static RESOLUTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(480p|540p|720p|1080p|1440p|2160p|4k|8k)\b").unwrap());
static CODEC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b(xvid|x264|h\.?264|avc|x265|h\.?265|hevc|av1)\b").unwrap());
static QUALITY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(WEB[ ._-]?DL|WEB[ ._-]?Rip|WEB|HDTV|Blu[ ._-]?Ray|BRRip|BDRip|HDRip|DVDRip|DVD[ ._-]?Rip|CAM|HDCAM|TS|HDTS|Telesync|Screener|DVDScr|R5|PPV|PDTV)\b").unwrap()
});
static AUDIO_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(DDP?5\.1|DD5\.1|AC3|AAC|DTS|TrueHD|Atmos|MP3|FLAC)\b").unwrap()
});

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedLibrary {
    pub shows: BTreeMap<String, ParsedShow>,
    pub movies: Vec<ParsedMovie>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedShow {
    pub key: String,
    pub title: String,
    pub seasons: BTreeMap<i32, ParsedSeason>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSeason {
    pub season: i32,
    pub episodes: Vec<ParsedEpisode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEpisode {
    pub file_id: String,
    pub relative_path: String,
    pub season: i32,
    pub episode: i32,
    pub episode_title: String,
    pub filename: String,
    pub quality: String,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMovie {
    pub file_id: String,
    pub title: String,
    pub year: i32,
    pub relative_path: String,
    pub filename: String,
    pub quality: String,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedName {
    pub title: String,
    pub season: Option<i32>,
    pub episodes: Vec<i32>,
    pub episode_name: String,
    pub year: Option<i32>,
    pub resolution: String,
    pub quality: String,
    pub codec: String,
    pub audio: String,
    pub encoder: String,
}

#[allow(dead_code)]
pub fn parse(name: &str, standardise: bool, _coherent_types: bool) -> Map<String, Value> {
    let parsed = parse_name(name, standardise);
    let mut out = Map::new();
    if !parsed.title.is_empty() {
        out.insert("title".to_string(), json!(parsed.title));
    }
    if let Some(season) = parsed.season {
        out.insert("season".to_string(), json!(season));
    }
    if !parsed.episodes.is_empty() {
        if parsed.episodes.len() == 1 {
            out.insert("episode".to_string(), json!(parsed.episodes[0]));
        } else {
            out.insert("episode".to_string(), json!(parsed.episodes));
        }
    }
    if let Some(year) = parsed.year {
        out.insert("year".to_string(), json!(year));
    }
    for (key, value) in [
        ("episodeName", parsed.episode_name),
        ("resolution", parsed.resolution),
        ("quality", parsed.quality),
        ("codec", parsed.codec),
        ("audio", parsed.audio),
        ("encoder", parsed.encoder),
    ] {
        if !value.is_empty() {
            out.insert(key.to_string(), json!(value));
        }
    }
    out
}

pub fn parse_directory_tree(root: &DirectoryNode) -> ParsedLibrary {
    let mut lib = ParsedLibrary::default();
    walk_node(root, "", &mut lib);
    for show in lib.shows.values_mut() {
        for season in show.seasons.values_mut() {
            season.episodes.sort_by(|a, b| {
                a.season
                    .cmp(&b.season)
                    .then(a.episode.cmp(&b.episode))
                    .then(a.filename.cmp(&b.filename))
            });
        }
    }
    lib.movies.sort_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then(a.year.cmp(&b.year))
            .then(a.filename.cmp(&b.filename))
    });
    lib
}

fn walk_node(node: &DirectoryNode, path_prefix: &str, lib: &mut ParsedLibrary) {
    let current_path = match &node.file {
        Some(file) if !file.name.is_empty() && !path_prefix.is_empty() => {
            format!("{path_prefix}/{}", file.name)
        }
        Some(file) if !file.name.is_empty() => file.name.clone(),
        _ => path_prefix.to_string(),
    };

    let mut candidates = Vec::new();
    let mut has_main = false;
    for file in &node.files {
        if file.file_type != "VIDEO" || file.size < MIN_VIDEO_SIZE_BYTES {
            continue;
        }
        let relative_path = if current_path.is_empty() {
            file.name.clone()
        } else {
            format!("{current_path}/{}", file.name)
        };
        let supplement = is_trailer_or_sample(&file.name);
        if !supplement {
            has_main = true;
        }
        candidates.push((file, relative_path, supplement));
    }

    for (file, relative_path, supplement) in candidates {
        if supplement && has_main {
            continue;
        }
        process_file(file, &relative_path, lib);
    }

    for child in &node.children {
        walk_node(child, &current_path, lib);
    }
}

fn process_file(file: &PutIoFile, relative_path: &str, lib: &mut ParsedLibrary) {
    let mut parsed = parse_name_with_context(&file.name, relative_path, true);
    if parsed.title.is_empty() || parsed.title.chars().all(|c| c.is_ascii_digit() || c.is_whitespace()) {
        if let Some(path_title) = infer_title_from_path(relative_path) {
            parsed.title = path_title;
        }
    }
    if parsed.title.is_empty() {
        return;
    }
    let quality = if parsed.resolution.is_empty() {
        parsed.quality.clone()
    } else {
        parsed.resolution.clone()
    };
    if parsed.season.is_some() || !parsed.episodes.is_empty() {
        add_tv_episode(file, relative_path, parsed, quality, lib);
    } else {
        lib.movies.push(ParsedMovie {
            file_id: file.id.to_string(),
            title: parsed.title,
            year: parsed.year.unwrap_or_default(),
            relative_path: relative_path.to_string(),
            filename: file.name.clone(),
            quality,
            source: if parsed.quality.is_empty() {
                parsed.encoder
            } else {
                parsed.quality
            },
        });
    }
}

fn add_tv_episode(
    file: &PutIoFile,
    relative_path: &str,
    parsed: ParsedName,
    quality: String,
    lib: &mut ParsedLibrary,
) {
    let season = parsed.season.unwrap_or(1);
    let episodes = if parsed.episodes.is_empty() {
        vec![0]
    } else {
        parsed.episodes.clone()
    };
    let key = normalize_show_key(&parsed.title);
    let show = lib.shows.entry(key.clone()).or_insert_with(|| ParsedShow {
        key,
        title: parsed.title.clone(),
        seasons: BTreeMap::new(),
    });
    let bucket = show.seasons.entry(season).or_insert_with(|| ParsedSeason {
        season,
        episodes: Vec::new(),
    });
    for episode in episodes {
        let file_id = if parsed.episodes.len() > 1 {
            format!("{}_e{episode}", file.id)
        } else {
            file.id.to_string()
        };
        bucket.episodes.push(ParsedEpisode {
            file_id,
            relative_path: relative_path.to_string(),
            season,
            episode,
            episode_title: parsed.episode_name.clone(),
            filename: file.name.clone(),
            quality: quality.clone(),
            group: parsed.encoder.clone(),
        });
    }
}

pub fn parse_name(name: &str, standardise: bool) -> ParsedName {
    parse_name_inner(name, None, standardise)
}

pub fn parse_name_with_context(name: &str, relative_path: &str, standardise: bool) -> ParsedName {
    parse_name_inner(name, Some(relative_path), standardise)
}

fn parse_name_inner(name: &str, relative_path: Option<&str>, standardise: bool) -> ParsedName {
    let mut work = EXT_RE.replace(name.trim(), "").to_string();
    work = work.replace('_', " ");
    let mut parsed = ParsedName::default();

    static SITE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^\[\s*([^\]]+?)\s*\]\s*-?\s*|^(?:www\.)?[\w-]+\.[\w-]+\s+-\s*").unwrap());
    if let Some(caps) = SITE_RE.captures(&work) {
        if let Some(m) = caps.get(1) {
            parsed.encoder = clean_title(m.as_str());
        } else if let Some(m) = caps.get(0) {
            parsed.encoder = clean_title(m.as_str());
        }
        work = work[caps.get(0).unwrap().end()..].to_string();
    }

    let mut title_end = work.len();

    if parsed.encoder.is_empty() {
        if let Some((left, group)) = work.rsplit_once('-') {
            let group_clean = clean_title(group);
            if !group_clean.is_empty() && group_clean.len() <= 32 {
                parsed.encoder = group_clean;
                title_end = title_end.min(left.len());
            }
        }
    }

    let mut extract_prop = |re: &Regex, standardise_fn: fn(&str) -> String| -> String {
        if let Some(caps) = re.captures(&work) {
            let m = caps.get(0).unwrap();
            let val = caps.get(1).map(|m| m.as_str()).unwrap_or(m.as_str());
            let std_val = if standardise { standardise_fn(val.trim()) } else { standardise_fn(val) };
            let blanks = " ".repeat(m.end() - m.start());
            title_end = title_end.min(m.start());
            work.replace_range(m.start()..m.end(), &blanks);
            std_val
        } else {
            String::new()
        }
    };

    parsed.resolution = extract_prop(&RESOLUTION_RE, |s| s.to_string());
    parsed.quality = extract_prop(&QUALITY_RE, standardise_quality);
    parsed.codec = extract_prop(&CODEC_RE, standardise_codec);
    parsed.audio = extract_prop(&AUDIO_RE, standardise_audio);

    if let Some(caps) = YEAR_RE.captures(&work) {
        parsed.year = caps.get(1).and_then(|m| m.as_str().parse().ok());
        let m = caps.get(0).unwrap();
        title_end = title_end.min(m.start());
        let blanks = " ".repeat(m.end() - m.start());
        work.replace_range(m.start()..m.end(), &blanks);
    }

    static GO_DASH_EP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\s-\s(\d{1,4})(?:\s-\s|\s+)(.*)$").unwrap());
    
    if let Some(caps) = SXX_EXX_RE.captures(&work) {
        let m = caps.get(0).unwrap();
        title_end = title_end.min(m.start());
        parsed.season = caps.get(1).and_then(|m| m.as_str().parse().ok());
        let first = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let second = caps.get(3).and_then(|m| m.as_str().parse().ok());
        parsed.episodes = expand_episode_range(first, second);
        parsed.episode_name = clean_title(&work[m.end()..]);
    } else if let Some(caps) = X_EP_RE.captures(&work) {
        let m = caps.get(0).unwrap();
        title_end = title_end.min(m.start());
        parsed.season = caps.get(1).and_then(|m| m.as_str().parse().ok());
        parsed.episodes = vec![caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)];
        parsed.episode_name = clean_title(&work[m.end()..]);
    } else if let Some(caps) = GO_DASH_EP_RE.captures(&work).or_else(|| DASH_EP_RE.captures(&work)) {
        let episode_match = caps.get(1).unwrap();
        if let Some(m0) = GO_DASH_EP_RE.captures(&work) {
            title_end = title_end.min(m0.get(0).unwrap().start());
        } else {
            title_end = title_end.min(episode_match.start().saturating_sub(1));
        }
        parsed.episodes = vec![episode_match.as_str().parse().unwrap_or(0)];
        parsed.season = relative_path.and_then(infer_season_from_path);
        parsed.episode_name = caps.get(2).map(|m| clean_title(m.as_str())).unwrap_or_default();
    } else {
        if let Some(caps) = SEASON_FOLDER_RE.captures(&work) {
            let m = caps.get(0).unwrap();
            title_end = title_end.min(m.start());
            parsed.season = caps.get(1).and_then(|m| m.as_str().parse().ok());
        } else if let Some(caps) = SHORT_SEASON_RE.captures(&work) {
            let m = caps.get(0).unwrap();
            title_end = title_end.min(m.start());
            parsed.season = caps.get(1).and_then(|m| m.as_str().parse().ok());
        }
    }

    if parsed.season.is_none() {
        parsed.season = relative_path.and_then(infer_season_from_path);
    }

    if parsed.episodes.is_empty() && parsed.season.is_some() {
        if let Some((start, end, m_start)) = infer_episode_range_from_name(&work) {
            if end >= start && end - start <= 500 {
                title_end = title_end.min(m_start);
                parsed.episodes = (start..=end).collect();
            }
        }
    }

    let raw_title = &work[..title_end];
    let title_no_brackets = EXCESS_RE.replace_all(raw_title, " ");
    parsed.title = clean_title(&title_no_brackets);

    if parsed.title.is_empty() {
        parsed.title = clean_title(raw_title);
    }
    
    if parsed.episode_name == parsed.encoder {
        parsed.episode_name = String::new();
    }

    parsed
}

fn infer_season_from_path(path: &str) -> Option<i32> {
    path.split('/')
        .rev()
        .find_map(|part| {
            SEASON_FOLDER_RE
                .captures(part)
                .or_else(|| SHORT_SEASON_RE.captures(part))
                .and_then(|caps| caps.get(1).and_then(|m| m.as_str().parse().ok()))
        })
}

fn infer_title_from_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop(); // remove filename
    for part in parts.into_iter().rev() {
        if SEASON_FOLDER_RE.is_match(part) || SHORT_SEASON_RE.is_match(part) || COMPLETE_RANGE_RE.is_match(part) {
            continue;
        }
        let parsed_dir = parse_name_inner(part, None, true);
        if !parsed_dir.title.is_empty() {
            return Some(parsed_dir.title);
        }
    }
    None
}

fn infer_episode_range_from_name(value: &str) -> Option<(i32, i32, usize)> {
    let caps = COMPLETE_RANGE_RE.captures(value)?;
    Some((
        caps.get(1)?.as_str().parse().ok()?,
        caps.get(2)?.as_str().parse().ok()?,
        caps.get(0)?.start(),
    ))
}

fn expand_episode_range(first: i32, second: Option<i32>) -> Vec<i32> {
    match second {
        Some(second) if second > first && second - first <= 20 => (first..=second).collect(),
        Some(second) => vec![first, second],
        None => vec![first],
    }
}


fn clean_title(value: &str) -> String {
    let mut s = value.replace(['.', '_', '+'], " ");
    s = s.replace(['[', ']', '(', ')'], " ");
    s = s.replace('-', " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn standardise_quality(value: &str) -> String {
    let compact = value.to_lowercase().replace([' ', '.', '_', '-'], "");
    match compact.as_str() {
        "webdl" | "webdlrip" | "hdrip" => "WEB-DL".to_string(),
        "webrip" | "web" => "WEBRip".to_string(),
        "bluray" => "Blu-ray".to_string(),
        "brrip" => "BRRip".to_string(),
        "bdrip" => "BDRip".to_string(),
        "dvdrip" | "dvdriprip" => "DVD-Rip".to_string(),
        "hdcam" | "cam" => "Cam".to_string(),
        "hdts" | "ts" | "telesync" => "Telesync".to_string(),
        "dvdscr" | "screener" => "Screener".to_string(),
        "hdtv" => "HDTV".to_string(),
        "ppv" => "Pay-Per-View Rip".to_string(),
        "pdtv" => "PDTV".to_string(),
        "r5" => "R5".to_string(),
        _ => value.to_string(),
    }
}

fn standardise_codec(value: &str) -> String {
    let compact = value.to_lowercase().replace([' ', '.', '_', '-'], "");
    match compact.as_str() {
        "x264" | "h264" | "avc" => "H.264".to_string(),
        "x265" | "h265" | "hevc" => "H.265".to_string(),
        "xvid" => "Xvid".to_string(),
        "av1" => "AV1".to_string(),
        _ => value.to_string(),
    }
}

fn standardise_audio(value: &str) -> String {
    let compact = value.to_lowercase().replace([' ', '_', '-'], "");
    match compact.as_str() {
        "dd5.1" | "ddp5.1" => "Dolby Digital 5.1".to_string(),
        "ac3" => "Dolby Digital".to_string(),
        "aac" => "AAC".to_string(),
        "dts" => "DTS".to_string(),
        "truehd" => "Dolby TrueHD".to_string(),
        "atmos" => "Dolby Atmos".to_string(),
        "mp3" => "MP3".to_string(),
        "flac" => "FLAC".to_string(),
        _ => value.to_string(),
    }
}

fn normalize_show_key(title: &str) -> String {
    title.trim().to_lowercase()
}

fn is_trailer_or_sample(filename: &str) -> bool {
    SUPPLEMENT_RE.is_match(filename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::putio::types::UnifiedDirectoryTree;

    #[test]
    fn parses_common_tv_episode() {
        let parsed = parse_name(
            "The.Walking.Dead.S05E03.1080p.WEB-DL.DD5.1.H.264-Cyphanix.mkv",
            true,
        );
        assert_eq!(parsed.title, "The Walking Dead");
        assert_eq!(parsed.season, Some(5));
        assert_eq!(parsed.episodes, vec![3]);
        assert_eq!(parsed.resolution, "1080p");
        assert_eq!(parsed.quality, "WEB-DL");
    }

    #[test]
    fn parses_common_movie() {
        let parsed = parse_name(
            "Hercules.2014.EXTENDED.1080p.WEB-DL.DD5.1.H264-RARBG.mkv",
            true,
        );
        assert_eq!(parsed.title, "Hercules");
        assert_eq!(parsed.year, Some(2014));
        assert_eq!(parsed.quality, "WEB-DL");
        assert_eq!(parsed.codec, "H.264");
    }

    #[test]
    fn parses_directory_tree_and_filters_samples() {
        let tree = UnifiedDirectoryTree {
            root: DirectoryNode {
                file: None,
                children: vec![],
                files: vec![
                    PutIoFile {
                        id: 1,
                        name: "Show.S01E01.720p.HDTV.x264-GRP.mkv".to_string(),
                        file_type: "VIDEO".to_string(),
                        size: 100 * 1024 * 1024,
                        ..PutIoFile::default()
                    },
                    PutIoFile {
                        id: 2,
                        name: "Show.S01E01.sample.mkv".to_string(),
                        file_type: "VIDEO".to_string(),
                        size: 80 * 1024 * 1024,
                        ..PutIoFile::default()
                    },
                    PutIoFile {
                        id: 3,
                        name: "Movie.2024.1080p.WEB-DL.mkv".to_string(),
                        file_type: "VIDEO".to_string(),
                        size: 120 * 1024 * 1024,
                        ..PutIoFile::default()
                    },
                ],
            },
            ..UnifiedDirectoryTree::default()
        };
        let lib = parse_directory_tree(&tree.root);
        assert_eq!(lib.shows.len(), 1);
        assert_eq!(lib.movies.len(), 1);
        let show = lib.shows.get("show").unwrap();
        assert_eq!(show.seasons[&1].episodes.len(), 1);
    }

    #[test]
    fn parses_anime_dash_episode_with_season_context() {
        let parsed = parse_name_with_context(
            "[Anime Time] Naruto - 142 - The Three Villains from the Maximum Security Prison.mkv",
            "chill.institute/[Anime Time] Naruto Complete (001-220 + Movies) [BD] [Dual Audio][1080p][HEVC 10bit x265][AAC][Eng Sub]/Season 04/[Anime Time] Naruto - 142 - The Three Villains from the Maximum Security Prison.mkv",
            true,
        );
        assert_eq!(parsed.title, "Naruto");
        assert_eq!(parsed.season, Some(4));
        assert_eq!(parsed.episodes, vec![142]);
        assert_eq!(
            parsed.episode_name,
            "The Three Villains from the Maximum Security Prison"
        );
    }

    #[test]
    fn parses_anime_dash_episode_without_season_context() {
        let parsed = parse_name("[Anime Time] Naruto - 142 - The Three Villains.mkv", true);
        assert_eq!(parsed.title, "Naruto");
        assert_eq!(parsed.season, None);
        assert_eq!(parsed.episodes, vec![142]);
    }

    #[test]
    fn directory_tree_classifies_anime_dash_episodes_as_tv() {
        let tree = UnifiedDirectoryTree {
            root: DirectoryNode {
                file: None,
                children: vec![DirectoryNode {
                    file: Some(PutIoFile {
                        id: 10,
                        name: "Season 04".to_string(),
                        file_type: "FOLDER".to_string(),
                        ..PutIoFile::default()
                    }),
                    children: vec![],
                    files: vec![PutIoFile {
                        id: 142,
                        name: "[Anime Time] Naruto - 142 - The Three Villains from the Maximum Security Prison.mkv".to_string(),
                        file_type: "VIDEO".to_string(),
                        size: 100 * 1024 * 1024,
                        ..PutIoFile::default()
                    }],
                }],
                files: vec![],
            },
            ..UnifiedDirectoryTree::default()
        };
        let lib = parse_directory_tree(&tree.root);
        assert_eq!(lib.movies.len(), 0);
        let show = lib.shows.get("naruto").unwrap();
        assert_eq!(show.title, "Naruto");
        assert_eq!(show.seasons[&4].episodes[0].episode, 142);
    }

    #[test]
    fn copied_fixture_files_are_present() {
        let inputs: Vec<String> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/input.json")).unwrap();
        let raw: Vec<Map<String, Value>> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/output_raw.json")).unwrap();
        let standard: Vec<Map<String, Value>> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/output_standard.json")).unwrap();
        assert_eq!(inputs.len(), 408);
        assert_eq!(raw.len(), inputs.len());
        assert_eq!(standard.len(), inputs.len());
    }

    #[test]
    fn fixture_subset_matches_known_cases() {
        let inputs: Vec<String> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/input.json")).unwrap();
        let raw: Vec<Map<String, Value>> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/output_raw.json")).unwrap();
        let standard: Vec<Map<String, Value>> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/output_standard.json")).unwrap();
        let cases: &[(usize, &[&str])] = &[
            (0, &["title", "season", "episode", "resolution", "quality", "codec"]),
            (1, &["title", "year", "resolution", "quality", "codec"]),
            (2, &["title", "year", "quality", "codec"]),
            (3, &["title", "season", "episode", "quality", "codec"]),
            (61, &["title", "season", "episode", "resolution", "quality", "codec"]),
            (150, &["title", "season", "episode"]),
            (362, &["title"]),
        ];
        for (index, keys) in cases {
            let got = parse(&inputs[*index], true, false);
            for key in *keys {
                let expected = standard[*index].get(*key).or_else(|| raw[*index].get(*key));
                assert_eq!(
                    got.get(*key),
                    expected,
                    "fixture {index} key {key} input {:?}",
                    inputs[*index]
                );
            }
        }
    }

    #[test]
    #[ignore = "full PTN fixture parity is tracked incrementally"]
    fn full_standard_fixture_parity() {
        let inputs: Vec<String> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/input.json")).unwrap();
        let standard: Vec<Map<String, Value>> =
            serde_json::from_str(include_str!("../tests/fixtures/fileparser/output_standard.json")).unwrap();
        for (index, input) in inputs.iter().enumerate() {
            let got = parse(input, true, false);
            for (key, expected_value) in &standard[index] {
                assert_eq!(got.get(key), Some(expected_value), "fixture {index} key {key}");
            }
        }
    }
}
