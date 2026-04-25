pub mod detect;

use std::path::PathBuf;

use crate::storage::config::ConfigStore;

pub fn active_path(config: &ConfigStore, detection: &detect::MpvDetection) -> Option<PathBuf> {
    match config.mpv_source().as_str() {
        "custom" => {
            let path = PathBuf::from(config.mpv_path());
            path.exists().then_some(path)
        }
        #[cfg(not(target_os = "windows"))]
        "managed" => detection.system.clone(),
        #[cfg(target_os = "windows")]
        "managed" => detection.managed.clone(),
        _ => detection.system.clone(),
    }
}
