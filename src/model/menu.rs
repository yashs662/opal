//! Right-click context-menu slice.
//!
//! Holds the open state, the logical-px position to anchor the menu at
//! (the cursor), and the right-clicked track's actionable data. Opening
//! requests a scene rebuild (like the other popups), so the menu renders
//! at the new position with the new target's actions; dismissing closes
//! it the same way.

/// The right-clicked track's data the menu acts on.
#[derive(Clone, Default)]
pub struct MenuTarget {
    /// `spotify:track:…` URI — Add to queue.
    pub uri: String,
    /// Album id — "Go to album" (empty hides the item).
    pub album_id: String,
    /// First-artist id — "Go to artist" (empty hides the item).
    pub artist_id: String,
    /// The full row, when the surface has it — enables "Add to
    /// playlist…" (the like picker needs title/cover/duration to
    /// live-patch open pages). `None` hides that item. Boxed so the
    /// target (which rides `Msg` by value) stays pointer-sized.
    pub track: Option<Box<crate::api::PlaylistTrack>>,
}

pub struct MenuModel {
    pub open: bool,
    /// Anchor position in **logical px** (cursor at right-click time).
    pub pos: [f32; 2],
    pub target: MenuTarget,
}

impl MenuModel {
    pub fn new() -> Self {
        Self {
            open: false,
            pos: [0.0; 2],
            target: MenuTarget::default(),
        }
    }

    pub fn show(&mut self, target: MenuTarget, pos: [f32; 2]) {
        self.target = target;
        self.pos = pos;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

impl Default for MenuModel {
    fn default() -> Self {
        Self::new()
    }
}
