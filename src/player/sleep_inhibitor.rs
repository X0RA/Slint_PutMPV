//! Cross-platform "keep system awake while video plays" wrapper around
//! `keepawake`. Held only while playback is actively progressing — pausing
//! or stopping releases the inhibitor so an unattended app doesn't keep
//! the laptop awake.

use keepawake::KeepAwake;
use tracing::warn;

pub struct SleepInhibitor {
    inner: Option<KeepAwake>,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Acquire the inhibitor if not already held. Idempotent.
    pub fn acquire(&mut self) {
        if self.inner.is_some() {
            return;
        }
        match keepawake::Builder::default()
            .display(true)
            .idle(true)
            .reason("Video playback")
            .app_name("PutMPV")
            .app_reverse_domain("io.github.x0ra.putmpv")
            .create()
        {
            Ok(handle) => self.inner = Some(handle),
            Err(e) => warn!("could not acquire sleep inhibitor: {e:?}"),
        }
    }

    /// Drop the inhibitor if held. Idempotent.
    pub fn release(&mut self) {
        self.inner = None;
    }
}
