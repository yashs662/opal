//! View layer.
//!
//! Each view owns its components, its callbacks, and its scene build; the
//! app's router state ([`View`]) selects which one is active. This is the
//! layer that replaces "`main` composes everything" — `main` only
//! constructs the views and dispatches to them.

pub mod home;
pub mod login;
pub mod setup;

/// Which top-level view is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Splash,
    /// First-run client-id setup (paste id + dashboard instructions). Shown
    /// when no client id is configured, and returned to by a prefs reset.
    Setup,
    Login,
    Home,
}

/// What the centre (main) pane of the Home view is showing. The sidebar,
/// now-playing pane, and player bar stay mounted across these; only the
/// main pane's content swaps (with a slide/fade transition). Switching is
/// a deliberate one-shot scene rebuild — distinct from the periodic
/// rebuilds the reactive path was built to avoid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MainNav {
    /// The default Home feed (greeting, recents, top artists, …).
    #[default]
    Home,
    /// A playlist detail page. `id` is the Spotify playlist id, or
    /// [`crate::api::LIKED_SONGS_ID`] when `liked` is set.
    Playlist { id: String, liked: bool },
    /// An album detail page. `id` is the Spotify album id. Reuses the
    /// playlist track-list machinery (an album has a `context_uri` + tracks).
    Album { id: String },
    /// An artist page. `id` is the Spotify artist id. Shows the artist hero
    /// + a discography grid of album tiles (each opens an [`Self::Album`]).
    Artist { id: String },
    /// A full-width "Show all" list for a home-feed section. Renders from the
    /// already-loaded `HomeData` (no fetch); rows open the matching detail.
    ShowAll { section: HomeSection },
    /// The active device's play queue (now playing + next up). Fetched
    /// fresh on every open — live state, no cache.
    Queue,
}

impl MainNav {
    /// Name of the open detail page's scroller node. Scoped to the page
    /// content: the engine preserves scroll state across scene rebuilds
    /// **by node name** (so e.g. the settings modal opening doesn't
    /// reset the list), which means pages that should start at the top
    /// on every navigation must not share one name across content.
    pub fn detail_scroll_node(&self) -> Option<String> {
        match self {
            MainNav::Playlist { id, .. } | MainNav::Album { id } => {
                Some(format!("detail_scroll:{id}"))
            }
            MainNav::Home | MainNav::Artist { .. } | MainNav::ShowAll { .. } | MainNav::Queue => {
                None
            }
        }
    }
}

/// Which home-feed section a [`MainNav::ShowAll`] page expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeSection {
    Recent,
    TopArtists,
    TopTracks,
}
