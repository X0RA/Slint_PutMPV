use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct MpvDetection {
    pub system: Option<PathBuf>,
    pub custom: Option<PathBuf>,
    pub managed: Option<PathBuf>,
}

impl MpvDetection {
    pub fn run(custom_path: Option<PathBuf>) -> Self {
        let custom = custom_path.and_then(|path| path.exists().then_some(path));
        Self {
            system: detect_system(),
            custom,
            managed: None,
        }
    }
}

#[cfg(target_os = "linux")]
fn detect_system() -> Option<PathBuf> {
    which::which("mpv").ok()
}

#[cfg(target_os = "macos")]
fn detect_system() -> Option<PathBuf> {
    let p = PathBuf::from("/Applications/IINA.app/Contents/MacOS/iina-cli");
    p.exists().then_some(p)
}

#[cfg(target_os = "windows")]
fn detect_system() -> Option<PathBuf> {
    which::which("mpvnet")
        .ok()
        .or_else(|| which::which("mpv").ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn detect_system() -> Option<PathBuf> {
    which::which("mpv").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_smoke_test() {
        let _ = MpvDetection::run(None);
    }
}
