//! Shared UI-thread and cross-thread state for the Slint bridge.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use crate::app_ui::metadata_ui::MetadataUiState;
use crate::putio::types::UnifiedDirectoryTree;

#[derive(Default)]
pub(crate) struct OauthFlow {
    pub cancel: Option<Arc<AtomicBool>>,
}

pub(crate) struct UiState {
    pub tree: Arc<RwLock<UnifiedDirectoryTree>>,
    pub sync_profiles: Arc<RwLock<Vec<crate::putio::sync::SyncProfile>>>,
    pub files_refreshing: Arc<AtomicBool>,
    pub auto_metadata_fetching: Arc<AtomicBool>,
    pub pending_local_clear: Rc<RefCell<Option<i32>>>,
    pub current_folder: Rc<RefCell<u64>>,
    pub path_stack: Rc<RefCell<Vec<(u64, String)>>>,
    pub oauth_flow: Rc<RefCell<OauthFlow>>,
    pub metadata_state: Rc<RefCell<MetadataUiState>>,
    pub auto_metadata_attempted: Rc<RefCell<HashSet<String>>>,
}

impl UiState {
    pub(crate) fn new() -> Self {
        Self {
            tree: Arc::new(RwLock::new(UnifiedDirectoryTree::default())),
            sync_profiles: Arc::new(RwLock::new(Vec::new())),
            files_refreshing: Arc::new(AtomicBool::new(false)),
            auto_metadata_fetching: Arc::new(AtomicBool::new(false)),
            pending_local_clear: Rc::new(RefCell::new(None)),
            current_folder: Rc::new(RefCell::new(0)),
            path_stack: Rc::new(RefCell::new(vec![(0u64, "put.io".to_string())])),
            oauth_flow: Rc::new(RefCell::new(OauthFlow::default())),
            metadata_state: Rc::new(RefCell::new(MetadataUiState::new())),
            auto_metadata_attempted: Rc::new(RefCell::new(HashSet::new())),
        }
    }
}
