//! Episode skip markers. Only Media playback supplies an EpisodeContext.
mod providers;

use serde::{Deserialize, Serialize};

pub use providers::Service;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MetadataSource {
    Tmdb,
    Tvmaze,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalIds {
    pub tvdb_id: Option<i32>,
    pub imdb_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpisodeContext {
    pub source: MetadataSource,
    pub series_id: i32,
    pub episode_id: i32,
    pub season: i32,
    pub episode: i32,
    pub title: String,
    pub anime: bool,
    pub ids: ExternalIds,
    pub file_version: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Intro,
    Recap,
    Credits,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub kind: Kind,
    pub start: f64,
    pub end: f64,
    pub source: String,
}

impl Segment {
    pub fn valid(&self, duration: f64) -> bool {
        if !duration.is_finite()
            || duration <= 0.0
            || !self.start.is_finite()
            || !self.end.is_finite()
            || self.start < 0.0
            || self.end > duration + 0.5
            || self.end - self.start < 2.0
        {
            return false;
        }
        match self.kind {
            Kind::Intro => {
                self.start < (duration * 0.5).min(1200.0) && self.end - self.start <= 300.0
            }
            Kind::Recap => {
                self.start < (duration * 0.25).min(600.0) && self.end - self.start <= 600.0
            }
            Kind::Credits => self.start >= duration * 0.65 && self.end - self.start <= 900.0,
        }
    }

    pub fn contains(&self, position: f64) -> bool {
        position.is_finite() && position >= self.start && position < self.end
    }

    pub fn terminal(&self, duration: f64) -> bool {
        (duration - self.end).abs() <= 2.0
    }
}

/// Keep a provider's complete segment boundaries; never average incompatible releases.
pub fn fill_missing(segments: &mut Vec<Segment>, candidates: Vec<Segment>, duration: f64) {
    let kinds: Vec<_> = segments.iter().map(|s| s.kind).collect();
    for candidate in candidates {
        if candidate.valid(duration)
            && !kinds.contains(&candidate.kind)
            && !segments
                .iter()
                .any(|s| candidate.start < s.end && s.start < candidate.end)
        {
            segments.push(candidate);
        }
    }
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
}

pub fn chapters(chapters: &[(String, f64)], duration: f64) -> Vec<Segment> {
    let mut segments = Vec::new();
    // Anime releases often label the cold open "Intro" and the actual theme "OP".
    let has_opening = chapters.iter().any(|(name, _)| {
        matches!(
            name.trim().to_lowercase().as_str(),
            "opening" | "opening credits" | "op" | "op1" | "op2"
        )
    });
    let has_ending = chapters.iter().any(|(name, _)| {
        matches!(
            name.trim().to_lowercase().as_str(),
            "ending" | "ending credits" | "end credits" | "ed" | "ed1" | "ed2"
        )
    });
    for (i, (title, start)) in chapters.iter().enumerate() {
        let name = title.trim().to_lowercase();
        if (has_opening && matches!(name.as_str(), "intro" | "introduction"))
            || (has_ending && matches!(name.as_str(), "outro" | "credits"))
        {
            continue;
        }
        // Exact, descriptive labels only. Generic numbered chapters are not evidence.
        let kind = match name.as_str() {
            "intro" | "introduction" | "opening" | "opening credits" | "op" | "op1" | "op2" => {
                Kind::Intro
            }
            "recap" | "previously" | "previously on" | "previously on…" => Kind::Recap,
            "credits" | "end credits" | "ending" | "ending credits" | "outro" | "ed" | "ed1"
            | "ed2" => Kind::Credits,
            _ => continue,
        };
        let segment = Segment {
            kind,
            start: *start,
            end: chapters.get(i + 1).map(|c| c.1).unwrap_or(duration),
            source: "Chapters".into(),
        };
        if segment.valid(duration) {
            segments.push(segment);
        }
    }
    segments
}

#[cfg(test)]
mod tests;
