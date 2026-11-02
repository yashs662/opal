//! Right-click context-menu slice — one menu for every right-click surface
//! (track rows, the window's close button), so they share the box, the
//! dismissal and the stacking.
//!
//! Holds the menu's [`Overlay`] (fade, outside-press dismiss, input
//! blocking and open-order stacking — so a popup launched from the menu
//! lands above whatever the menu itself was opened over), the logical-px
//! position to anchor it at (the cursor), and what was right-clicked
//! ([`MenuKind`]), which decides the items. Opening requests a scene rebuild so the menu renders at
//! the new position with the new target's actions.

use std::time::{Duration, Instant};

use opal_gfx::{Overlay, Timeline};

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

impl MenuTarget {
    /// The canonical builder — every track surface (rows, cards, recents)
    /// routes through this so the menu offers the same items everywhere:
    /// Add to queue, Go to album/artist (when known), Add to playlist….
    pub fn for_track(t: &crate::api::PlaylistTrack) -> Self {
        Self {
            uri: t.uri.clone(),
            album_id: t.album_id.clone(),
            artist_id: t.artist_id.clone(),
            track: Some(Box::new(t.clone())),
        }
    }
}

/// What the menu was opened on — decides its items.
#[derive(Clone)]
pub enum MenuKind {
    /// A track row: queue / radio / album / artist / add to playlist.
    Track(MenuTarget),
    /// The window's close button while the lossless engine runs: close
    /// Opal and keep the official client playing, or close as usual.
    Window,
}

pub struct MenuModel {
    /// Quick-fade, transparent-scrim overlay — owns open state + dismiss.
    pub overlay: Overlay,
    /// Anchor position in **logical px** (cursor at right-click time).
    pub pos: [f32; 2],
    pub kind: MenuKind,
}

impl MenuModel {
    pub fn new() -> Self {
        Self {
            overlay: Overlay::new().with_fade(Duration::from_millis(90)),
            pos: [0.0; 2],
            kind: MenuKind::Window,
        }
    }

    pub fn show(&mut self, kind: MenuKind, pos: [f32; 2], tl: &mut Timeline, now: Instant) {
        self.kind = kind;
        self.pos = pos;
        self.overlay.open(tl, now);
    }
}

impl Default for MenuModel {
    fn default() -> Self {
        Self::new()
    }
}
