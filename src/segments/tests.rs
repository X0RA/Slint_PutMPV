use super::*;
#[test]
fn markers_reject_invalid_ranges_and_reappear_when_seeking_back() {
    let mut marker = Segment {
        kind: Kind::Intro,
        start: 30.0,
        end: 90.0,
        source: "test".into(),
    };
    assert!(marker.valid(1500.0));
    assert!(!marker.contains(29.0));
    assert!(marker.contains(30.0));
    assert!(!marker.contains(90.0));
    assert!(marker.contains(45.0));
    assert!(!marker.contains(f64::NAN));
    marker.end = f64::INFINITY;
    assert!(!marker.valid(1500.0));
    marker.end = 20.0;
    assert!(!marker.valid(1500.0));
    marker.kind = Kind::Credits;
    marker.start = 100.0;
    marker.end = 150.0;
    assert!(!marker.valid(1500.0));
}
#[test]
fn chapters_respect_post_credit_scenes_and_ignore_generic_labels() {
    let markers = chapters(
        &[
            ("Chapter 1".into(), 0.0),
            ("Opening".into(), 90.0),
            ("Episode".into(), 180.0),
            ("Ending".into(), 1300.0),
            ("After credits".into(), 1390.0),
        ],
        1440.0,
    );
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0].end, 180.0);
    assert!(!markers[1].terminal(1440.0));
}
#[test]
fn fallbacks_fill_missing_types_without_merging_conflicting_boundaries() {
    let mut segments = vec![Segment {
        kind: Kind::Intro,
        start: 30.0,
        end: 90.0,
        source: "chapter".into(),
    }];
    fill_missing(
        &mut segments,
        vec![
            Segment {
                kind: Kind::Intro,
                start: 50.0,
                end: 110.0,
                source: "api".into(),
            },
            Segment {
                kind: Kind::Recap,
                start: 10.0,
                end: 40.0,
                source: "api".into(),
            },
            Segment {
                kind: Kind::Credits,
                start: 1400.0,
                end: 1500.0,
                source: "api".into(),
            },
        ],
        1500.0,
    );
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].end, 90.0);
    assert_eq!(segments[1].kind, Kind::Credits);
}

#[test]
fn explicit_theme_chapter_takes_priority_over_a_cold_open_named_intro() {
    let markers = chapters(
        &[
            ("Intro".into(), 0.033),
            ("OP".into(), 239.01),
            ("Part A".into(), 328.98),
            ("ED".into(), 1286.01),
            ("Outro".into(), 1375.94),
        ],
        1396.0,
    );
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0].start, 239.01);
    assert!(!markers.iter().any(|s| s.contains(60.0)));
    assert!(!markers[1].terminal(1396.0));
}
