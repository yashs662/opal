//! Reactive player-chrome state — the now-playing + player-bar surface.
//!
//! Holds the live track title/artist plus the transport toggles
//! (`is_playing`/`shuffle`/`repeat_on`) and `progress`, all as reactive
//! signals so a cluster push updates the chrome via the lib's binds with
//! **no scene rebuild**: title/artist → text binds, `is_playing` →
//! play/pause image bind, `shuffle`/`repeat_on` → tint colour binds,
//! `progress` (0..=1) → bar fill width.
//!
//! This is the UI-facing reactive mirror; the authoritative
//! [`crate::api::CurrentlyPlaying`] snapshot still lives on the app state.

use std::time::{Duration, Instant};

use opal_gfx::{Curve, Signal, TextSignal, Timeline};

use crate::api::{CurrentlyPlaying, RepeatMode};

pub struct PlayerModel {
    pub title: TextSignal,
    pub artist: TextSignal,
    /// Display name of the queue source ("Chill", "Daily Mix 2", …) from
    /// the cluster's context metadata, falling back to the context *kind*
    /// when unnamed — the now-playing pane's top-left source pill.
    /// Reactive so a track change updates it without a rebuild.
    pub context_label: TextSignal,
    /// Whether the playing context is known at all (a uri or a name) —
    /// hides the source pill entirely instead of showing a bare "—" disc
    /// when the state carries neither (fresh cold start, contextless play).
    pub context_known: Signal<bool>,
    /// Web-API-resolved fallback name for a context uri whose pushes carry
    /// no `context_description` (the `/me/player` cold-start seed, our own
    /// local playback): `(uri, name)`. An empty name is a negative entry —
    /// resolution failed / unfetchable kind — so the reducer doesn't
    /// re-dispatch on every push. [`Self::sync`] prefers it over the bare
    /// kind label.
    pub resolved_context: Option<(String, String)>,
    /// Context uri with a name fetch in flight (dedup, like
    /// [`np_about_inflight`](Self::np_about_inflight)).
    pub context_inflight: Option<String>,
    /// "About the artist" data for the current track's first artist —
    /// fetched by the worker on artist change; `None` hides the section.
    pub np_about: Option<crate::api::ArtistDetail>,
    /// The artist's biography paragraph (extended metadata), shown under
    /// the follower count. Arrives with `np_about`; `None` = no bio.
    pub np_bio: Option<String>,
    /// Artist id with a card fetch in flight — dedups re-dispatch on the
    /// cluster's non-track pushes (play/pause/seek/volume).
    pub np_about_inflight: Option<String>,
    /// Credits for the current track (`(track_id, credits)`) — the
    /// card's "Credits" section. The id keys staleness: a set from the
    /// previous track never renders under the next one.
    pub np_credits: Option<(String, crate::worker::TrackCredits)>,
    /// Track id with a credits fetch in flight (dedup).
    pub np_credits_inflight: Option<String>,
    /// Height (logical px) of the now-playing pane's above-card block:
    /// scroller viewport − card peek, measured by the frame tick, so the
    /// card's accent header rests at the pane's bottom edge until scrolled.
    pub np_fill_h: Signal<f32>,
    /// Now-playing pane width (logical px) — measured pane height ×
    /// 9/16 by the frame tick, so the pane always holds the Canvas
    /// aspect as the window resizes.
    pub np_pane_w: Signal<f32>,
    /// Scroll progress (0..=1) of the now-playing card reveal, derived from
    /// the pane's spring-smoothed scroll offset by the frame tick. Drives
    /// the card's glassy-dark → full-accent colour blend; smooth for free
    /// because the scroll offset itself is spring-interpolated.
    pub np_card_t: Signal<f32>,
    pub is_playing: Signal<bool>,
    pub shuffle: Signal<bool>,
    pub repeat_on: Signal<bool>,
    /// True only in the Track (repeat-one) mode — drives the player-bar
    /// glyph swap between the repeat-all and repeat-1 icons. `repeat_on`
    /// still gates the accent tint (on for both Context and Track).
    pub repeat_track: Signal<bool>,
    /// Playback progress as a fraction of the track (0.0..=1.0).
    pub progress: Signal<f32>,
    /// Current track duration (ms), as a signal so the seek tooltip's
    /// target-timestamp updates on track change without a rebuild.
    pub duration_ms: Signal<f32>,
    /// Seek-bar interaction state (the scrubbable progress bar):
    /// `seek_preview` is the cursor's fraction along the bar (0..=1) on
    /// hover/drag → drives the fill while dragging + the tooltip position
    /// & label; `bar_hovered`/`seeking` gate the tooltip's visibility and
    /// select fill source. Commit happens on release (see `tick_seek`).
    pub seek_preview: Signal<f32>,
    /// Cursor X along the bar in **logical px** (clamped to the lane). Drives
    /// the tooltip's composite layer offset so it follows the cursor without
    /// dirtying layout (see the seek-bar's `layer_offset_x`).
    pub seek_preview_px: Signal<f32>,
    pub bar_hovered: Signal<bool>,
    pub seeking: Signal<bool>,
    /// Formatted target timestamp ("M:SS") at `seek_preview` — the seek
    /// tooltip label. Updated by [`SeekHandle::set_at`].
    pub seek_label: TextSignal,
    /// Live elapsed ("M:SS", left of the bar) + total duration ("M:SS",
    /// right). `elapsed_label` is updated once per second by
    /// [`Self::tick_clock`]; `total_label` on track change in [`Self::sync`].
    pub elapsed_label: TextSignal,
    pub total_label: TextSignal,
    /// Last whole-second elapsed pushed to `elapsed_label`. The clock tick
    /// runs **every frame for the whole song**, so this guard avoids building
    /// a `String` 60×/s for a label that only changes once a second. (The
    /// seek tooltip needs no such guard — it's interaction-only and
    /// `TextSignal::set` already dedups the re-flatten.)
    last_elapsed_secs: u32,
    /// Last observed `seeking` value, for release-edge detection in
    /// [`Self::tick_seek`].
    seek_held_last: bool,
    /// Active device volume as a 0..=1 fraction — drives the volume
    /// slider fill. Updated by `VolumeChanged` (local mixer event or
    /// cluster push) unless the user is mid-drag.
    pub volume: Signal<f32>,
    /// Volume slider held — gates incoming `VolumeChanged` so a stale
    /// confirmation doesn't fight the drag.
    pub vol_dragging: Signal<bool>,
    /// Current track is in the user's Liked Songs — heart tint. Checked
    /// on every track change; flipped optimistically on click with the
    /// worker echo as the authority.
    pub liked: Signal<bool>,
    /// Volume slider hovered — shows the percentage tooltip.
    pub vol_hovered: Signal<bool>,
    /// Cursor X along the volume slider (**logical px**, clamped) — drives
    /// the tooltip's composite offset so the "NN%" pill follows the
    /// cursor, exactly like the seek tooltip.
    pub vol_preview_px: Signal<f32>,
    /// "NN%" tooltip label — the value at the cursor while hovering/dragging
    /// the volume slider (set by [`VolumeHandle`]).
    pub vol_label: TextSignal,
    /// Authoritative live player snapshot (the latest cluster push). The
    /// reactive signals above are the UI mirror; this is the source of
    /// truth handlers read for the current track/cover/repeat mode.
    pub snapshot: Option<CurrentlyPlaying>,
    /// Whether a real live push has landed this session. False while the
    /// snapshot is only the cold-start seed (persisted last track) — lets the
    /// play button know nothing is actually playing yet, so it resumes the
    /// last track explicitly instead of a bare Web API resume that no-ops.
    pub live: bool,
    /// The Connect (Spirc) session has registered Opal as a device, so
    /// transport can actually act on something. False on cold start until
    /// `SpotifySessionConnected` (or `…Failed`, the Web-API fallback) — the
    /// player bar shows a pulsing "loading" play button and swallows clicks
    /// until then, so an early press isn't a silent no-op.
    pub session_ready: Signal<bool>,
    /// Opacity of the black "loading" overlay (black circle + brand logo) that
    /// sits over the play button while `!session_ready`. 1 while loading, then
    /// tweened to 0 so the black background + logo dissolve out together,
    /// revealing the real play/pause button beneath.
    pub loading_fade: Signal<f32>,
    /// Opacity pulse for the logo within the overlay — ping-pong tweened while
    /// loading so it breathes; parked once ready.
    pub loading_pulse: Signal<f32>,
    /// Edge guard so the tick starts/stops the loading animation on the
    /// readiness transition, not every frame.
    loading_anim_on: bool,
}

impl PlayerModel {
    /// Seed from the cold-start snapshot (persisted `last_player` +
    /// `audio.volume`), so the chrome renders the last-played track and
    /// volume immediately instead of a dash and a default. `progress` is the
    /// fraction (0..=1); `progress_ms`/`duration_ms` seed the elapsed/total
    /// labels so the bar isn't paired with a bogus `0:00 / 0:00` until the
    /// first live push (which may never come if nothing is playing).
    pub fn seed(
        title: &str,
        artist: &str,
        progress: f32,
        progress_ms: u64,
        duration_ms: u64,
        volume: f32,
        restored: Option<CurrentlyPlaying>,
    ) -> Self {
        Self {
            title: TextSignal::new(title),
            artist: TextSignal::new(artist),
            context_label: TextSignal::new(
                restored
                    .as_ref()
                    .and_then(|p| p.context_name.as_deref())
                    .unwrap_or_else(|| {
                        context_kind(restored.as_ref().and_then(|p| p.context_uri.as_deref()))
                    }),
            ),
            context_known: Signal::new(
                restored
                    .as_ref()
                    .map(|p| p.context_uri.is_some() || p.context_name.is_some())
                    .unwrap_or(false),
            ),
            resolved_context: None,
            context_inflight: None,
            np_about: None,
            np_bio: None,
            np_about_inflight: None,
            np_credits: None,
            np_credits_inflight: None,
            np_fill_h: Signal::new(480.0),
            np_pane_w: Signal::new(304.0),
            np_card_t: Signal::new(0.0),
            is_playing: Signal::new(false),
            shuffle: Signal::new(false),
            repeat_on: Signal::new(false),
            repeat_track: Signal::new(false),
            progress: Signal::new(progress),
            duration_ms: Signal::new(duration_ms as f32),
            seek_preview: Signal::new(0.0),
            seek_preview_px: Signal::new(0.0),
            bar_hovered: Signal::new(false),
            seeking: Signal::new(false),
            seek_label: TextSignal::new("0:00"),
            elapsed_label: TextSignal::new(fmt_ms(progress_ms.min(duration_ms) as u32).as_str()),
            total_label: TextSignal::new(fmt_ms(duration_ms as u32).as_str()),
            last_elapsed_secs: u32::MAX,
            seek_held_last: false,
            volume: Signal::new(volume.clamp(0.0, 1.0)),
            vol_dragging: Signal::new(false),
            liked: Signal::new(false),
            vol_hovered: Signal::new(false),
            vol_preview_px: Signal::new(0.0),
            vol_label: TextSignal::new(format!(
                "{}%",
                (volume.clamp(0.0, 1.0) * 100.0).round() as u32
            )),
            // The cold-start seed counts as the *current* track (paused) so
            // the heart/membership/canvas checks fire on launch — see
            // `AppState::from_prefs`. The first live cluster push overwrites it.
            snapshot: restored,
            live: false,
            session_ready: Signal::new(false),
            // 0 = overlay hidden; armed to 1 on the first loading tick, so a
            // session that's already ready when Home mounts shows no overlay.
            loading_fade: Signal::new(0.0),
            loading_pulse: Signal::new(1.0),
            loading_anim_on: false,
        }
    }

    // --- snapshot access ----------------------------------------------
    // The authoritative `snapshot` cell is read/written from many handlers.
    // These accessors keep every borrow scoped to a single call so a
    // double-borrow can't be introduced by accident, and give the call
    // sites intent-named methods instead of `snapshot.borrow().as_ref()…`.

    /// Read the live snapshot under a short borrow, mapping it to `T`.
    /// `None` when nothing is loaded.
    pub fn with_snapshot<T>(&self, f: impl FnOnce(&CurrentlyPlaying) -> T) -> Option<T> {
        self.snapshot.as_ref().map(f)
    }

    /// The current track's `spotify:track:…` uri, if a snapshot is loaded.
    pub fn current_track_uri(&self) -> Option<String> {
        self.snapshot.as_ref().map(|p| p.track_id.clone())
    }

    /// Whether a snapshot is loaded at all (something is playing or was
    /// restored from the cold-start seed).
    pub fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Clone the whole live snapshot — for rollback paths that need the
    /// full struct back.
    pub fn snapshot_clone(&self) -> Option<CurrentlyPlaying> {
        self.snapshot.clone()
    }

    /// Replace the live snapshot (an authoritative cluster push landed).
    pub fn set_snapshot(&mut self, snapshot: Option<CurrentlyPlaying>) {
        self.snapshot = snapshot;
    }

    /// Mutate the live snapshot in place, if one is loaded.
    pub fn patch_snapshot<T>(&mut self, f: impl FnOnce(&mut CurrentlyPlaying) -> T) -> Option<T> {
        self.snapshot.as_mut().map(f)
    }

    /// Set the volume fraction + its "NN%" tooltip label together — the
    /// write point for incoming `VolumeChanged` confirmations.
    pub fn set_volume_ui(&self, frac: f32) {
        let frac = frac.clamp(0.0, 1.0);
        self.volume.set(frac);
        self.vol_label.set(fmt_pct(frac).as_str());
    }

    /// Cloneable write-handle for the volume slider's `'static` event
    /// closures (which can't borrow the model) — the volume analogue of
    /// [`Self::seek_handle`].
    pub fn volume_handle(&self) -> VolumeHandle {
        VolumeHandle {
            volume: self.volume.clone(),
            preview_px: self.vol_preview_px.clone(),
            label: self.vol_label.clone(),
        }
    }

    /// Push a live player snapshot into the reactive chrome. All sets are
    /// dedup'd by the signal layer, so a same-track progress tick only
    /// bumps what changed. Progress snaps to the live position, then (if
    /// playing) tweens to 1.0 over the remaining duration so the bar
    /// advances smoothly between cluster pushes; paused stops the tween so
    /// the bar holds.
    pub fn sync(&mut self, p: &CurrentlyPlaying, tl: &mut Timeline, now: Instant) {
        self.live = true;
        // A live push means there's something to act on (a remote device is
        // playing even before our own Connect session registers) — enable
        // transport + drop the loading pulse.
        self.session_ready.set(true);
        self.title.set(p.name.as_str());
        self.artist.set(p.artist.as_str());
        // Source-pill label: the push's own display name, else the
        // Web-API-resolved fallback for this uri, else the bare kind.
        let label = p.context_name.as_deref().or_else(|| {
            self.resolved_context
                .as_ref()
                .filter(|(u, n)| Some(u.as_str()) == p.context_uri.as_deref() && !n.is_empty())
                .map(|(_, n)| n.as_str())
        });
        self.context_label
            .set(label.unwrap_or_else(|| context_kind(p.context_uri.as_deref())));
        self.context_known
            .set(p.context_uri.is_some() || p.context_name.is_some());
        self.is_playing.set(p.is_playing);
        self.shuffle.set(p.shuffle);
        self.repeat_on.set(!matches!(p.repeat, RepeatMode::Off));
        self.repeat_track.set(matches!(p.repeat, RepeatMode::Track));
        self.duration_ms.set(p.duration_ms as f32);
        self.total_label.set(fmt_ms(p.duration_ms as u32).as_str());

        let live = p.live_progress_ms().min(p.duration_ms);
        let frac = if p.duration_ms > 0 {
            live as f32 / p.duration_ms as f32
        } else {
            0.0
        };
        self.progress.set(frac);
        if p.is_playing && p.duration_ms > 0 {
            let remaining = p.duration_ms.saturating_sub(live);
            tl.animate(
                &self.progress,
                1.0,
                Curve::Linear,
                Duration::from_millis(remaining),
                now,
            );
        } else {
            tl.stop_for(&self.progress);
        }
    }

    /// Nothing playing on any device. Don't wipe the chrome to a dash —
    /// keep the last track visible, just mark stopped and freeze the bar.
    pub fn stopped(&self, tl: &mut Timeline) {
        self.is_playing.set(false);
        tl.stop_for(&self.progress);
    }

    /// Drive the play button's loading pulse from the frame tick: ping-pong
    /// the opacity while the player bar is showing (`relevant`) and the
    /// session isn't ready yet; park at 1.0 otherwise. Edge-guarded so it
    /// doesn't re-arm (or keep the loop awake) once settled.
    pub fn tick_loading(&mut self, relevant: bool, tl: &mut Timeline, now: Instant) {
        let loading = relevant && !self.session_ready.get();
        if loading == self.loading_anim_on {
            return;
        }
        self.loading_anim_on = loading;
        if loading {
            // Fade the black+logo overlay in over the play button + breathe
            // the logo.
            tl.animate(
                &self.loading_fade,
                1.0,
                Curve::EaseInOut,
                Duration::from_millis(220),
                now,
            );
            tl.animate_pingpong(
                &self.loading_pulse,
                1.0,
                0.4,
                Curve::EaseInOut,
                Duration::from_millis(800),
                now,
            );
        } else {
            // Ready — dissolve the overlay (black bg + logo) out together,
            // revealing the real play/pause button.
            tl.animate(
                &self.loading_fade,
                0.0,
                Curve::EaseInOut,
                Duration::from_millis(420),
                now,
            );
            tl.stop_for(&self.loading_pulse);
            self.loading_pulse.set(1.0);
        }
    }

    // --- optimistic transport (player-bar intents) --------------------
    //
    // Each flips the optimistic UI signal immediately and returns the
    // domain value the host maps to a worker command — the dealer cluster
    // push corrects the real state shortly after. Returning bool/RepeatMode
    // (not a worker command type) keeps this slice free of `worker`.

    /// Toggle play/pause optimistically; returns whether it **was** playing
    /// (so the host sends Pause if it was, else Play).
    pub fn toggle_play(&self) -> bool {
        let was_playing = self.is_playing.get();
        self.is_playing.set(!was_playing);
        was_playing
    }

    /// Toggle shuffle optimistically; returns the new state.
    pub fn toggle_shuffle(&self) -> bool {
        let next = !self.shuffle.get();
        self.shuffle.set(next);
        next
    }

    /// Advance the repeat mode Off → Context → Track → Off (Spotify's
    /// cycle), driven off the live snapshot's actual mode (not just the
    /// `repeat_on` bool) so the three-state cycle is correct. Updates the
    /// optimistic toggle tint and returns the new mode.
    pub fn cycle_repeat(&self) -> RepeatMode {
        let current = self
            .snapshot
            .as_ref()
            .map(|p| p.repeat)
            .unwrap_or(RepeatMode::Off);
        let next = match current {
            RepeatMode::Off => RepeatMode::Context,
            RepeatMode::Context => RepeatMode::Track,
            RepeatMode::Track => RepeatMode::Off,
        };
        self.repeat_on.set(!matches!(next, RepeatMode::Off));
        self.repeat_track.set(matches!(next, RepeatMode::Track));
        next
    }

    /// Refresh the elapsed-time label from the live `progress` fraction.
    /// Called each frame; re-formats (and dirties the text) only when the
    /// whole-second value changes, so the smoothly-tweening progress doesn't
    /// re-flatten the bar every frame.
    pub fn tick_clock(&mut self) {
        let secs = (self.progress.get() * self.duration_ms.get() / 1000.0).max(0.0) as u32;
        if secs != self.last_elapsed_secs {
            self.last_elapsed_secs = secs;
            self.elapsed_label.set(fmt_ms(secs * 1000).as_str());
        }
    }

    // --- seek bar -----------------------------------------------------

    /// A cloneable write-handle for the bar's `'static` event closures
    /// (which can't borrow the model). Updates the preview fraction +
    /// tooltip label from the cursor's position.
    pub fn seek_handle(&self) -> SeekHandle {
        SeekHandle {
            preview: self.seek_preview.clone(),
            preview_px: self.seek_preview_px.clone(),
            label: self.seek_label.clone(),
            duration_ms: self.duration_ms.clone(),
        }
    }

    /// Run from the frame loop. On the release edge of a seek drag (the
    /// `seeking` signal falling true→false), snap the bar to the previewed
    /// position (so it doesn't jump back to the live tween mid-flight) and
    /// return the absolute target position (ms) for the host to dispatch as
    /// a Web API seek. The dealer cluster push re-syncs shortly after.
    pub fn tick_seek(&mut self, tl: &mut Timeline) -> Option<u32> {
        let held = self.seeking.get();
        let was_held = self.seek_held_last;
        self.seek_held_last = held;
        if was_held && !held {
            let frac = self.seek_preview.get().clamp(0.0, 1.0);
            let dur = self.duration_ms.get();
            if dur > 0.0 {
                self.progress.set(frac);
                tl.stop_for(&self.progress);
                return Some((frac * dur) as u32);
            }
        }
        None
    }
}

/// Cloneable seek-bar write handle (see [`PlayerModel::seek_handle`]).
#[derive(Clone)]
pub struct SeekHandle {
    preview: Signal<f32>,
    preview_px: Signal<f32>,
    label: TextSignal,
    duration_ms: Signal<f32>,
}

impl SeekHandle {
    /// Set the preview from the cursor's X relative to the bar's left
    /// (`x_rel`, logical px) given the lane width (`lane_w`, px). Updates the
    /// fraction (0..=1, for the fill), the px offset (for the tooltip's
    /// composite layer position), and the timestamp label. `TextSignal::set`
    /// dedups, so a same-second label is a no-op that doesn't dirty the tree;
    /// the only per-move cost is building the short `M:SS` string, and only
    /// while the user is actively hovering the bar.
    pub fn set_at(&self, x_rel: f32, lane_w: f32) {
        let w = lane_w.max(1.0);
        let frac = (x_rel / w).clamp(0.0, 1.0);
        self.preview.set(frac);
        self.preview_px.set(x_rel.clamp(0.0, w));
        let ms = (frac * self.duration_ms.get()) as u32;
        self.label.set(fmt_ms(ms).as_str());
    }
}

/// Cloneable volume-slider write handle (see [`PlayerModel::volume_handle`]).
/// Mirrors [`SeekHandle`]: `preview_at` moves only the tooltip (hover —
/// "what clicking here sets"); `set_at` also moves the fill (drag).
#[derive(Clone)]
pub struct VolumeHandle {
    volume: Signal<f32>,
    preview_px: Signal<f32>,
    label: TextSignal,
}

impl VolumeHandle {
    /// Hover preview: position the tooltip at the cursor + show the
    /// fraction it would set, **without** moving the actual volume.
    pub fn preview_at(&self, x_rel: f32, lane_w: f32) {
        let w = lane_w.max(1.0);
        let frac = (x_rel / w).clamp(0.0, 1.0);
        self.preview_px.set(x_rel.clamp(0.0, w));
        self.label.set(fmt_pct(frac).as_str());
    }

    /// Drag/click: set the volume to the cursor fraction (fill follows
    /// 1:1) and move the tooltip with it.
    pub fn set_at(&self, x_rel: f32, lane_w: f32) {
        let w = lane_w.max(1.0);
        let frac = (x_rel / w).clamp(0.0, 1.0);
        self.volume.set(frac);
        self.preview_px.set(x_rel.clamp(0.0, w));
        self.label.set(fmt_pct(frac).as_str());
    }

    /// Position the tooltip at a fraction along the bar (used by the wheel
    /// path, where there's no cursor-along-bar — the pill rides the thumb).
    pub fn label_at_frac(&self, frac: f32, lane_w: f32) {
        let frac = frac.clamp(0.0, 1.0);
        self.preview_px.set(frac * lane_w.max(1.0));
        self.label.set(fmt_pct(frac).as_str());
    }
}

/// Human label for a playing context uri — the card's "Playing from" row.
fn context_kind(uri: Option<&str>) -> &'static str {
    match uri {
        Some(u) if u.ends_with(":collection") => "Liked Songs",
        Some(u) if u.contains(":album:") => "Album",
        Some(u) if u.contains(":playlist:") => "Playlist",
        Some(u) if u.contains(":artist:") => "Artist",
        Some(u) if u.contains(":show:") => "Podcast",
        Some(_) => "Queue",
        None => "\u{2014}",
    }
}

/// Format a millisecond position as `M:SS` for the seek tooltip.
fn fmt_ms(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Format a 0..=1 fraction as `NN%` for the volume tooltip.
fn fmt_pct(frac: f32) -> String {
    format!("{}%", (frac.clamp(0.0, 1.0) * 100.0).round() as u32)
}
