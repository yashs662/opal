//! `Msg` — the single typed-intent enum for the TEA migration, plus the
//! transitional [`MsgQueue`] the view pushes into.
//!
//! View callbacks emit a `Msg` into the queue; the frame tick drains it and
//! [`crate::app::update::update`] applies each one to the models with the
//! frame's real [`Cx`](crate::app::cx::Cx). This is the TEA shape: callbacks
//! capture only the queue (not `AppState`), and every mutation runs in one
//! place at a well-defined point in the frame — no event-time interior
//! mutation scattered across closures.
//!
//! The `Rc<RefCell<…>>` queue is *transitional* — it goes away at Stage 2,
//! when `opal_gfx` owns the message queue and hands `&mut AppState` to
//! `update` directly. Until then it's one shared cell replacing dozens of
//! state-capturing callbacks: net progress off interior mutability. See
//! `PLAN_TEA.md`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use opal_gfx::WakeHandle;

use crate::api::PlayTarget;
use crate::model::MenuTarget;
use crate::prefs::AudioQuality;
use crate::views::MainNav;
use crate::views::home::PlayerAction;

/// Intents emitted by the view this frame, drained by the frame tick.
pub type MsgQueue = Rc<RefCell<VecDeque<Msg>>>;

/// Build an empty message queue.
pub fn queue() -> MsgQueue {
    Rc::new(RefCell::new(VecDeque::new()))
}

/// The view's handle for emitting intents. Wraps the shared [`MsgQueue`] plus
/// the loop [`WakeHandle`]: [`Dispatch::send`] enqueues a `Msg` **and** wakes
/// the loop, so the frame tick's `on_frame` hook fires and drains it this same
/// cycle. (Without the wake, a click that only pushes a `Msg` would sit
/// undrained until some other wake — the loop parks on `Wait` when idle.)
#[derive(Clone)]
pub struct Dispatch {
    queue: MsgQueue,
    wake: Arc<WakeHandle>,
}

impl Dispatch {
    pub fn new(queue: MsgQueue, wake: Arc<WakeHandle>) -> Self {
        Self { queue, wake }
    }

    /// Queue an intent and wake the loop to drain it.
    pub fn send(&self, msg: Msg) {
        self.queue.borrow_mut().push_back(msg);
        self.wake.wake();
    }
}

/// A view-emitted intent. Applied by [`crate::app::update::update`].
pub enum Msg {
    /// Centre-pane navigation (feed ⇄ playlist / album / artist / queue /
    /// show-all).
    Navigate(MainNav),
    /// A transport control from the player bar (play/pause, next, shuffle, …).
    Transport(PlayerAction),
    /// Start playback of a resolved context (tile / row click).
    Play(PlayTarget),
    /// Lazily fetch a row/tile cover that just scrolled into view.
    RequestCover(String),
    /// Toggle the Spotify Canvas video on/off for the current track.
    CanvasToggle,
    /// Sign out → drop the session and return to Login.
    SignOut,
    /// Settings modal opened → re-scan disk-cache usage.
    SettingsOpen,
    /// Devices popup opened → fetch the live device list.
    DevicesOpen,
    /// Like picker opened → point it at the current track.
    LikeOpen,
    /// Add/remove the current track to/from a playlist (`add`).
    LikeTogglePlaylist { playlist_id: String, add: bool },
    /// Like/unlike the current track (Liked Songs).
    LikeToggleLiked(bool),
    /// Transfer playback to another Connect device.
    Transfer(String),
    /// Change the streaming-quality preference.
    SetQuality(AudioQuality),
    /// Persist the (already-flipped) "normalize volume" toggle.
    ToggleNormalize,
    /// Skip forward `count` tracks (queue "play this next-N").
    Skip(u32),
    /// Open the track-row right-click menu at `pos` (logical px).
    OpenContextMenu { pos: [f32; 2], target: MenuTarget },
    /// Add a track to the play queue.
    AddQueue(String),
    /// Close the right-click menu.
    MenuClose,
    /// Wipe the disk cache (off-thread).
    ClearCache,
    /// Open the folder picker to relocate the disk cache.
    ChangeCacheDir,
    /// Mark preferences dirty so the debounced save persists them.
    MarkDirty,
    /// Begin the OAuth login flow (login-screen button).
    StartLogin,
    /// Return from Login to the Setup screen to edit the client id.
    BackToSetup,
    /// Wipe prefs + tokens and return to Setup ("Reset preferences").
    ResetPrefs,
    /// Persist a validated client id and advance to Login (setup save).
    SaveClientId(String),
}
