//! Pure helpers for formatting and MPV/settings mapping (no Slint window types).

use tracing::warn;

pub(crate) fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

pub(crate) fn format_updated(ts: &Option<String>) -> String {
    let Some(s) = ts else {
        return String::new();
    };
    if s.len() < 10 {
        return s.clone();
    }
    let date = &s[..10];
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return date.to_string();
    }
    let year = parts[0];
    let month: usize = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = months
        .get(month.saturating_sub(1))
        .copied()
        .unwrap_or("???");
    format!("{m} {day:02}, {year}")
}

pub(crate) fn truncate_id(id: u64) -> i32 {
    if id > i32::MAX as u64 {
        warn!("put.io id {id} exceeds i32::MAX; truncating");
    }
    (id & 0x7FFF_FFFF) as i32
}

pub(crate) fn stable_i32_id(value: &str) -> i32 {
    let mut hash: u32 = 2_166_136_261;
    for b in value.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash & 0x7fff_ffff) as i32
}

pub(crate) fn make_initials(title: &str) -> slint::SharedString {
    let s: String = title
        .split_whitespace()
        .filter(|w| {
            w.chars()
                .next()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
        })
        .take(3)
        .map(|w| w.chars().next().unwrap().to_uppercase().to_string())
        .collect::<Vec<_>>()
        .join("");
    s.as_str().into()
}

pub(crate) fn source_to_index(source: &str) -> i32 {
    match source {
        "putio" | "custom" => 1,
        "managed" => 2,
        _ => 0,
    }
}

pub(crate) fn mpv_source_from_index(index: i32) -> &'static str {
    match index {
        1 => "custom",
        2 => "managed",
        _ => "system",
    }
}

pub(crate) fn path_label(path: &std::path::Path) -> String {
    path.to_string_lossy().to_string()
}

pub(crate) fn install_hint() -> (String, bool, &'static str) {
    #[cfg(target_os = "linux")]
    {
        (
            "MPV not found. Install it with your distro package manager, for example pacman -S mpv or apt install mpv.".to_string(),
            false,
            "",
        )
    }
    #[cfg(target_os = "macos")]
    {
        (
            "IINA not found. Download and install IINA from iina.io.".to_string(),
            true,
            "https://iina.io/",
        )
    }
    #[cfg(target_os = "windows")]
    {
        (
            "mpv.net not found. Download mpv.net from the latest releases page.".to_string(),
            true,
            "https://github.com/mpvnet-player/mpv.net/releases/latest",
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        (
            "MPV not found. Install MPV and configure a custom binary path.".to_string(),
            false,
            "",
        )
    }
}

pub(crate) fn format_runtime(minutes: i32) -> String {
    if minutes <= 0 {
        return String::new();
    }
    let h = minutes / 60;
    let m = minutes % 60;
    match (h > 0, m > 0) {
        (true, true) => format!("{h}h {m}m"),
        (true, false) => format!("{h}h"),
        _ => format!("{m}m"),
    }
}
