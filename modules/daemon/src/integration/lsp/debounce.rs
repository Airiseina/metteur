//! Per-document sync cooldown for LSP diagnostics.
//!
//! Agents edit code in rapid bursts: a single tool call may rewrite several
//! files, and the model frequently chains edits before looking at the result.
//! Pushing every intermediate revision to the language server wastes work and
//! returns diagnostics that are about to be invalidated.
//!
//! The cooldown merges syncs of the same document inside a short window: the
//! newest text is kept, and callers that arrive during the window wait for the
//! in-flight sync instead of starting another. An explicit request (an LLM
//! call or a validation node) always syncs immediately, because its whole
//! purpose is to observe the current state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The outcome of requesting a document sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    /// Sync now: the cooldown elapsed, or the caller asked to bypass it.
    SyncNow,
    /// An identical sync happened within the cooldown; reuse its diagnostics.
    ReuseRecent,
}

/// Tracks the last sync time per document.
#[derive(Debug, Default)]
pub struct SyncCooldown {
    last_sync: HashMap<String, Instant>,
    window: Duration,
}

impl SyncCooldown {
    /// Creates a cooldown with the given merge window (`0` disables merging).
    pub fn new(window: Duration) -> Self {
        Self {
            last_sync: HashMap::new(),
            window,
        }
    }

    /// Decides whether a sync for `uri` should run now.
    ///
    /// `explicit` marks a caller that must observe the current state (an LLM
    /// tool call or a validation node) and therefore bypasses the window.
    pub fn decide(&mut self, uri: &str, now: Instant, explicit: bool) -> SyncDecision {
        if explicit || self.window.is_zero() {
            self.last_sync.insert(uri.to_string(), now);
            return SyncDecision::SyncNow;
        }
        match self.last_sync.get(uri) {
            Some(last) if now.duration_since(*last) < self.window => SyncDecision::ReuseRecent,
            _ => {
                self.last_sync.insert(uri.to_string(), now);
                SyncDecision::SyncNow
            }
        }
    }

    /// Records a sync performed by another path (e.g. an explicit request) so
    /// a following implicit request still merges.
    pub fn record(&mut self, uri: &str, now: Instant) {
        self.last_sync.insert(uri.to_string(), now);
    }

    /// Forgets a document (its file was deleted or the manager was rebuilt).
    pub fn forget(&mut self, uri: &str) {
        self.last_sync.remove(uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> SyncCooldown {
        SyncCooldown::new(Duration::from_millis(300))
    }

    #[test]
    fn first_request_syncs() {
        let mut cooldown = window();
        let now = Instant::now();
        assert_eq!(cooldown.decide("file:///a.rs", now, false), SyncDecision::SyncNow);
    }

    #[test]
    fn repeat_within_the_window_reuses_recent_diagnostics() {
        let mut cooldown = window();
        let start = Instant::now();
        assert_eq!(cooldown.decide("file:///a.rs", start, false), SyncDecision::SyncNow);
        let soon = start + Duration::from_millis(100);
        assert_eq!(cooldown.decide("file:///a.rs", soon, false), SyncDecision::ReuseRecent);
    }

    #[test]
    fn request_after_the_window_syncs_again() {
        let mut cooldown = window();
        let start = Instant::now();
        cooldown.decide("file:///a.rs", start, false);
        let later = start + Duration::from_millis(400);
        assert_eq!(cooldown.decide("file:///a.rs", later, false), SyncDecision::SyncNow);
    }

    #[test]
    fn explicit_requests_bypass_the_window() {
        let mut cooldown = window();
        let start = Instant::now();
        cooldown.decide("file:///a.rs", start, false);
        // An LLM tool call or validation node must observe the current state.
        let soon = start + Duration::from_millis(10);
        assert_eq!(cooldown.decide("file:///a.rs", soon, true), SyncDecision::SyncNow);
    }

    #[test]
    fn documents_are_tracked_independently() {
        let mut cooldown = window();
        let start = Instant::now();
        cooldown.decide("file:///a.rs", start, false);
        assert_eq!(
            cooldown.decide("file:///b.rs", start, false),
            SyncDecision::SyncNow,
            "a different file must not inherit the window"
        );
    }

    #[test]
    fn a_zero_window_disables_merging() {
        let mut cooldown = SyncCooldown::new(Duration::ZERO);
        let now = Instant::now();
        assert_eq!(cooldown.decide("file:///a.rs", now, false), SyncDecision::SyncNow);
        assert_eq!(cooldown.decide("file:///a.rs", now, false), SyncDecision::SyncNow);
    }

    #[test]
    fn forget_clears_the_window() {
        let mut cooldown = window();
        let start = Instant::now();
        cooldown.decide("file:///a.rs", start, false);
        cooldown.forget("file:///a.rs");
        let soon = start + Duration::from_millis(10);
        assert_eq!(cooldown.decide("file:///a.rs", soon, false), SyncDecision::SyncNow);
    }
}
