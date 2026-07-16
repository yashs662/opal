//! Search slice — the Spotlight-style modal's state: the query mirror, the
//! latest results, the recent-search history, and the open/expand animation
//! signals.
//!
//! The modal opens over everything (an [`Overlay`] owns the scrim, dismiss,
//! and fade); a spring-tweened `panel_h` grows the panel out of the search
//! bar as results come in. The field's `on_change` mirrors into `query` and
//! stamps `dirty_since`; the frame tick dispatches the debounced fetch.
//! Results are tagged with the query they answer so a late response is
//! dropped in the reducer.

use std::time::Instant;

use opal_gfx::Overlay;
use serde::{Deserialize, Serialize};

use crate::api::SearchResults;

/// One recent-search entry — a result the user actually opened. Persisted,
/// so history survives restarts. Clicking it reopens that detail page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchHistoryEntry {
    /// `artist` / `track` / `album` / `playlist`.
    pub kind: String,
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub image_url: Option<String>,
}

/// Most-recent recent searches kept.
const HISTORY_CAP: usize = 10;

pub struct SearchModel {
    /// Scrim / dismiss / fade owner for the modal.
    pub overlay: Overlay,
    /// Live query text (mirrors the modal's field).
    pub query: String,
    /// Set when `query` last changed; cleared when the debounced search
    /// dispatches. `None` = nothing pending.
    pub dirty_since: Option<Instant>,
    /// The query a fetch was last dispatched for — dedups repeat dispatches
    /// and lets the reducer drop stale responses.
    pub dispatched: String,
    /// Latest results (for `dispatched`); `None` before the first search.
    pub results: Option<SearchResults>,
    /// Recent opened results (newest first), mirrored to prefs.
    pub history: Vec<SearchHistoryEntry>,
    /// Request focus on the modal field on the next build (set on open).
    pub focus_pending: bool,
}

impl SearchModel {
    pub fn new(history: Vec<SearchHistoryEntry>) -> Self {
        Self {
            // Screen-centred + height-morphing: the overlay owns the panel
            // height and springs it collapsed↔content on open/close/dismiss,
            // so it grows/shrinks from the middle in every direction.
            overlay: Overlay::new().with_morph(crate::views::home::search_modal::collapsed_h()),
            query: String::new(),
            dirty_since: None,
            dispatched: String::new(),
            results: None,
            history,
            focus_pending: false,
        }
    }

    /// Record an opened result at the top of the history (dedup by id, cap).
    /// Returns whether the history changed (so the caller persists).
    pub fn record(&mut self, entry: SearchHistoryEntry) -> bool {
        self.history.retain(|e| e.id != entry.id);
        self.history.insert(0, entry);
        self.history.truncate(HISTORY_CAP);
        true
    }

    /// Remove the history entry at `index` (a per-row ×). Returns changed.
    pub fn remove(&mut self, index: usize) -> bool {
        if index < self.history.len() {
            self.history.remove(index);
            true
        } else {
            false
        }
    }

    /// Clear all history. Returns whether anything was there.
    pub fn clear(&mut self) -> bool {
        let had = !self.history.is_empty();
        self.history.clear();
        had
    }
}
