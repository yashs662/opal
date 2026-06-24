//! The Home view — the main shell (top bar, sidebar, centre pane,
//! now-playing pane, player bar, settings modal). `build` assembles the
//! ambient backdrop + glass + splitter row and dispatches to the six
//! sub-component `.view()`s; each component owns its own slice.

pub mod artist;
pub mod context_menu;
pub mod devices;
pub mod like_menu;
pub mod main_pane;
pub mod now_playing;
pub mod player_bar;
pub mod playlist;
pub mod queue;
pub mod settings;
pub mod show_all;
pub mod sidebar;
pub mod top_bar;

use std::cell::RefCell;
use std::rc::Rc;
use opal_gfx::{Computed, EventCtx, ImageHandle, Len, Scene, Signal};

use crate::album_art;
use crate::api::PlayTarget;
use crate::app::AppState;
use crate::app::cx::Cx;
use crate::app::msg::{Dispatch, Msg};
use crate::views::{HomeSection, MainNav};
use crate::widgets::component::Component;
use crate::widgets::crossfade::OPAQUE_TINT;
use crate::widgets::icon::IconSet;
use crate::widgets::tokens as t;
use crate::worker::Worker;

/// Centre-pane navigation callback — opens a playlist or returns Home.
/// Takes the `EventCtx` so it can start the entrance transition tween at
/// click time.
pub type NavFn = Rc<dyn Fn(&mut EventCtx, MainNav)>;

/// Playback-start callback — hands a resolved [`PlayTarget`] up to the
/// consumer (which adds the token + dispatches the worker command).
pub type PlayFn = Rc<dyn Fn(PlayTarget)>;

/// Right-click handler for a track row — opens the context menu at the
/// cursor with the given target's actions. Takes the `EventCtx` for the
/// cursor position + display scale.
pub type CtxMenuFn = Rc<dyn Fn(&mut EventCtx, crate::model::MenuTarget)>;

/// Wire the right-click context menu (Add to queue / Go to album / artist)
/// onto a row or tile builder. The single source of truth for the gesture —
/// every track list and tile (playlist, queue, home feed, show-all, …) calls
/// this, so the menu behaves identically everywhere instead of each view
/// re-implementing the `on_right_click` + clone dance.
pub fn attach_context_menu(
    row: &mut opal_gfx::NodeBuilderRef<'_>,
    on_context_menu: &CtxMenuFn,
    target: crate::model::MenuTarget,
) {
    let menu = on_context_menu.clone();
    row.on_right_click(move |ctx| menu(ctx, target.clone()));
}

/// A transport intent raised by a player-bar button click. The consumer
/// (main.rs) maps these to optimistic signal flips + worker commands;
/// the UI layer stays ignorant of tokens and the Web API.
#[derive(Debug, Clone, Copy)]
pub enum PlayerAction {
    PlayPause,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    /// Commit the volume slider's released position (0..=100).
    SetVolume(u8),
}

/// Per-build layout inputs — the ambient backdrop signals + splitter
/// widths the shell `render` binds, plus refs to the constructed
/// sub-components. Built fresh each rebuild by [`HomeView::build`].
struct Layout<'a> {
    pub backdrop_prev: &'a Signal<Option<ImageHandle>>,
    pub backdrop_curr: &'a Signal<Option<ImageHandle>>,
    /// Slow backdrop + accent crossfade progress.
    pub crossfade_t: &'a Signal<f32>,
    /// Mean luminance of the current cover — drives the adaptive
    /// ambient-glass dim.
    pub art_luma: &'a Signal<f32>,
    /// Resizable panel widths (driven by splitters via `width_px_bind`).
    pub sidebar_w: &'a Signal<f32>,
    pub now_playing_w: &'a Signal<f32>,
    /// Called by the splitters after every committed width change.
    /// Wired by the consumer to debounced prefs persistence.
    pub mark_dirty: std::rc::Rc<dyn Fn()>,
    /// The now-playing pane, a self-rendering [`Component`] reading its
    /// own backdrop/player/canvas slices.
    pub now_playing: &'a crate::views::home::now_playing::NowPlaying<'a>,
    /// The left "Your Library" sidebar, a self-rendering [`Component`].
    pub sidebar: &'a crate::views::home::sidebar::Sidebar<'a>,
    /// The bottom player bar, a self-rendering [`Component`].
    pub player_bar: &'a crate::views::home::player_bar::PlayerBar<'a>,
    /// The top chrome bar (search + window controls), a [`Component`].
    pub top_bar: &'a crate::views::home::top_bar::TopBar<'a>,
    /// The centre pane (Home feed / playlist page), a [`Component`].
    pub main_pane: &'a crate::views::home::main_pane::MainPane<'a>,
    /// The settings modal, a [`Component`] owning its `Overlay` wrapper.
    pub settings_panel: &'a crate::views::home::settings::SettingsPanel<'a>,
    /// The Connect-devices popup, ditto.
    pub devices_panel: &'a crate::views::home::devices::DevicesPanel<'a>,
    /// The playlist-picker popup behind the like icon, ditto.
    pub like_menu: &'a crate::views::home::like_menu::LikeMenu<'a>,
    /// Right-click context menu (track row actions) + its handlers.
    pub menu: &'a crate::model::MenuModel,
    pub on_menu_add_queue: Rc<dyn Fn(String)>,
    pub on_menu_navigate: NavFn,
    pub on_menu_close: Rc<dyn Fn()>,
}

fn render(s: &mut Scene, v: &Layout) {
    // `home_root` itself is transparent (emits no instance — the
    // transparency skip drops it), so the back-most composite layer is the
    // `home_bg` fill below, which `main.rs` hides once the opaque album-art
    // backdrop fully covers it (no wasted full-screen draw behind the art).
    s.col("home_root").fill().child(|root| {
        // Base background fill. Toggled off (→ no instance, no layer) by
        // `tick_canvas_dim`/the backdrop watcher once the art covers it.
        root.rect("home_bg")
            .abs(0.0, 0.0)
            .w(Len::Fill)
            .h(Len::Fill)
            .rgba(t::BG[0], t::BG[1], t::BG[2], 1.0);
        // Outgoing layer: previous cover, held fully opaque so the
        // incoming layer dissolves over solid coverage (no background
        // bleed at the midpoint — see `fade_in_alpha`). Bound to the
        // signal via `image_bound`, so `promote_backdrop` swaps the
        // handle with no scene rebuild; `None` renders nothing (the
        // first track has no previous cover).
        // Gate the outgoing layer to `None` once the crossfade settles
        // (`crossfade_t == 1`): the incoming cover is then fully opaque
        // and covers it, so drawing it is a wasted per-frame draw call.
        let backdrop_prev_gated = Computed::new(
            (v.backdrop_prev.clone(), v.crossfade_t.clone()),
            |(p, t)| if t >= 1.0 { None } else { p },
        );
        root.image_bound((), backdrop_prev_gated)
            .abs(0.0, 0.0)
            .w(Len::Fill)
            .h(Len::Fill)
            .image_cover()
            .blur_source()
            .color(OPAQUE_TINT);
        // Incoming layer: current cover, fading in over the outgoing
        // one. **Composite-opacity crossfade (compositor P4):** the
        // image is held opaque (`OPAQUE_TINT`) and promoted to its own
        // layer via `.layer_opacity(crossfade_t)` — the lib drives the
        // layer's *composite* opacity from the tween each frame, so the
        // incoming cover's texture rasters **once** and the fade is a
        // composite-only recomposite (no per-frame image re-raster).
        // Generic glass (P4) sources its backdrop from the composite of
        // the layers below it, so the glass still blurs the dissolving
        // result. `blur_source` keeps the (still per-frame, inherent)
        // backdrop blur firing while the composite changes.
        root.image_bound((), v.backdrop_curr.clone())
            .abs(0.0, 0.0)
            .w(Len::Fill)
            .h(Len::Fill)
            .image_cover()
            .blur_source()
            .layer_opacity(v.crossfade_t.clone())
            .color(OPAQUE_TINT);
        // Frosted-glass overlay: heavy blur + dark tint = the dimmed
        // ambient look. Always present in Home — before any art it
        // just blurs the dark BG (reads the same), and keeping it
        // unconditional means the first cover appears *under* the
        // glass without needing a rebuild to introduce it.
        // The tint adapts to the cover's brightness: a near-white cover
        // would otherwise lift the whole backdrop to mid-grey and wash
        // out every icon/label above it, so bright art gets a
        // proportionally deeper dim — the chrome's background stays
        // predictably dark, which is what the contrast-lifted accent is
        // calibrated against. Reactive colour bind riding the slow
        // crossfade tween — re-tints once per track change.
        let glass_tint = Computed::new((v.art_luma.clone(),), |(l,)| {
            [0.0, 0.0, 0.0, 0.25 + 0.40 * l.clamp(0.0, 1.0)]
        });
        root.glass(())
            .abs(0.0, 0.0)
            .w(Len::Fill)
            .h(Len::Fill)
            .blur(80.0)
            .color(glass_tint);
        v.top_bar.view(root);
        root.row(())
            .w(Len::Fill)
            .h(Len::Fill)
            .pad(t::SP_2)
            .gap(t::SP_0)
            .child(|b| {
                v.sidebar.view(b);
                crate::widgets::splitter::splitter(
                    b,
                    crate::widgets::splitter::SplitterProps {
                        name: "split_sidebar",
                        width: v.sidebar_w.clone(),
                        side: crate::widgets::splitter::PanelSide::Left,
                        min: t::SIDEBAR_MIN,
                        max: t::SIDEBAR_MAX,
                        collapsed: t::SIDEBAR_COLLAPSED,
                        on_change: v.mark_dirty.clone(),
                    },
                );
                v.main_pane.view(b);
                crate::widgets::splitter::splitter(
                    b,
                    crate::widgets::splitter::SplitterProps {
                        name: "split_now_playing",
                        width: v.now_playing_w.clone(),
                        side: crate::widgets::splitter::PanelSide::Right,
                        min: t::NOW_PLAYING_MIN,
                        max: t::NOW_PLAYING_MAX,
                        collapsed: t::SP_0,
                        on_change: v.mark_dirty.clone(),
                    },
                );
                v.now_playing.view(b);
            });
        v.player_bar.view(root);
        // Modals — rendered last (layer on top), components that own
        // their Overlay wrappers. Skipped entirely when closed.
        v.settings_panel.view(root);
        v.devices_panel.view(root);
        v.like_menu.view(root);
        // Right-click context menu — topmost; renders only when open.
        context_menu::view(
            root,
            v.menu,
            v.on_menu_add_queue.clone(),
            v.on_menu_navigate.clone(),
            v.on_menu_close.clone(),
        );
    });
}

/// The Home view controller — owns the callbacks (built once in
/// [`HomeView::new`]) and, on each rebuild, constructs the sub-components
/// and assembles the scene ([`HomeView::build`]). This is the composition
/// that used to live in `main`.
pub struct HomeView {
    state: Rc<RefCell<AppState>>,
    icons: Rc<IconSet>,
    on_action: Rc<dyn Fn(PlayerAction)>,
    on_canvas_change: Rc<dyn Fn()>,
    sign_out: Rc<dyn Fn()>,
    on_settings_open: Rc<dyn Fn()>,
    on_clear_cache: Rc<dyn Fn()>,
    on_change_cache_dir: Rc<dyn Fn()>,
    on_navigate: NavFn,
    on_play: PlayFn,
    request_cover: playlist::CoverFn,
    mark_dirty: Rc<dyn Fn()>,
    on_devices_open: Rc<dyn Fn()>,
    on_like_open: Rc<dyn Fn()>,
    on_like_toggle_playlist: Rc<dyn Fn(String, bool)>,
    on_like_toggle_liked: Rc<dyn Fn(bool)>,
    on_transfer: Rc<dyn Fn(String)>,
    on_quality: Rc<dyn Fn(crate::prefs::AudioQuality)>,
    on_normalize: Rc<dyn Fn()>,
    on_skip: Rc<dyn Fn(u32)>,
    on_context_menu: CtxMenuFn,
    on_add_queue: Rc<dyn Fn(String)>,
    on_menu_close: Rc<dyn Fn()>,
}

impl HomeView {
    /// Build the view + all its event callbacks once. Post-TEA the callbacks
    /// capture only the `msgs` queue (not the models / worker / rebuild),
    /// pushing typed intents the frame tick drains through `app::update`.
    pub fn new(state: Rc<RefCell<AppState>>, dispatch: Dispatch, icons: Rc<IconSet>) -> Self {
        // TEA emitters: each host callback now just pushes a typed `Msg`
        // onto the queue (capturing only the queue, not the models). The
        // frame tick drains them through `app::update`, which holds the
        // actual logic. See PLAN_TEA.md.
        let on_action: Rc<dyn Fn(PlayerAction)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |action| dispatch.send(Msg::Transport(action)))
        };
        let on_canvas_change: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::CanvasToggle))
        };
        let sign_out: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::SignOut))
        };
        let on_settings_open: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::SettingsOpen))
        };
        let on_devices_open: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::DevicesOpen))
        };
        let on_like_open: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::LikeOpen))
        };
        let on_like_toggle_playlist: Rc<dyn Fn(String, bool)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |playlist_id, add| {
                dispatch.send(Msg::LikeTogglePlaylist { playlist_id, add })
            })
        };
        let on_like_toggle_liked: Rc<dyn Fn(bool)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |add| dispatch.send(Msg::LikeToggleLiked(add)))
        };
        let on_transfer: Rc<dyn Fn(String)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |device_id| dispatch.send(Msg::Transfer(device_id)))
        };
        let on_quality: Rc<dyn Fn(crate::prefs::AudioQuality)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |q| dispatch.send(Msg::SetQuality(q)))
        };
        let on_normalize: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::ToggleNormalize))
        };
        let on_skip: Rc<dyn Fn(u32)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |count| dispatch.send(Msg::Skip(count)))
        };
        let on_context_menu: CtxMenuFn = {
            let dispatch = dispatch.clone();
            Rc::new(move |ctx, target| {
                // Read the cursor at emit time (physical px → logical, as the
                // menu's `.abs()` expects) and carry it in the intent.
                let scale = ctx.tree.scale().max(1.0);
                let pos = [ctx.cursor[0] / scale, ctx.cursor[1] / scale];
                dispatch.send(Msg::OpenContextMenu { pos, target });
            })
        };
        let on_add_queue: Rc<dyn Fn(String)> = {
            let dispatch = dispatch.clone();
            Rc::new(move |uri| dispatch.send(Msg::AddQueue(uri)))
        };
        let on_menu_close: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::MenuClose))
        };
        let on_clear_cache: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::ClearCache))
        };
        let on_change_cache_dir: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::ChangeCacheDir))
        };
        let on_navigate: NavFn = {
            // Stage-1 TEA: the host callback is now a pure emitter — it pushes
            // the intent onto the queue (capturing only the queue, not the
            // models). The frame tick drains it through `app::update` with the
            // frame's `Cx`. See PLAN_TEA.md.
            let dispatch = dispatch.clone();
            Rc::new(move |_ctx, nav| dispatch.send(Msg::Navigate(nav)))
        };
        let request_cover: playlist::CoverFn = {
            let dispatch = dispatch.clone();
            Rc::new(move |url| dispatch.send(Msg::RequestCover(url)))
        };
        let on_play: PlayFn = {
            let dispatch = dispatch.clone();
            Rc::new(move |target| dispatch.send(Msg::Play(target)))
        };
        let mark_dirty: Rc<dyn Fn()> = {
            let dispatch = dispatch.clone();
            Rc::new(move || dispatch.send(Msg::MarkDirty))
        };
        Self {
            state,
            icons,
            on_action,
            on_canvas_change,
            sign_out,
            on_settings_open,
            on_clear_cache,
            on_change_cache_dir,
            on_navigate,
            on_play,
            request_cover,
            mark_dirty,
            on_devices_open,
            on_like_open,
            on_like_toggle_playlist,
            on_like_toggle_liked,
            on_transfer,
            on_quality,
            on_normalize,
            on_skip,
            on_context_menu,
            on_add_queue,
            on_menu_close,
        }
    }

    /// Assemble the Home scene: build the view data + sub-components from
    /// the live model and hand them to the shell `render`.
    pub fn build(&self, s: &mut Scene) {
        // Shared root borrow for the whole build (the read phase) — never
        // overlaps the tick's `borrow_mut` (distinct frame-loop passes).
        let state = self.state.borrow();
        let state = &*state;
        let icons = &self.icons;
        let nav = &state.router.nav;
        // Hold the home borrow for the whole build (read-only feed data).
        // The art model is passed by reference and looked up narrowly per
        // tile (`art.signal`) — deliberately NOT a held `home_art` borrow,
        // so a `borrow_mut` reached during the build can't double-borrow.
        let home_ref = &state.library.home;
        // Both playlist + album pages render through the playlist view (an
        // album is a track list with a context_uri); the hero label differs.
        let kind_label = match nav {
            MainNav::Album { .. } => "Album",
            _ => "Playlist",
        };
        let playlist: Option<playlist::PlaylistViewData> = match nav {
            MainNav::Playlist { .. } | MainNav::Album { .. } => {
                state.library.open_playlist.as_ref().map(|o| {
                    let cover = o
                        .image_url
                        .as_ref()
                        .and_then(|u| state.art.signal(&album_art::cache_key(u)));
                    playlist::PlaylistViewData {
                        name: o.name.clone(),
                        owner: o.owner.clone(),
                        total: o.total,
                        liked: o.liked,
                        kind_label,
                        loading: o.loading,
                        cover,
                        context_uri: o.context_uri.clone(),
                        rows: o.rows.clone(),
                        request_cover: self.request_cover.clone(),
                        pulse: state.library.skeleton_pulse.clone(),
                        on_context_menu: self.on_context_menu.clone(),
                    }
                })
            }
            MainNav::Home | MainNav::Artist { .. } | MainNav::ShowAll { .. } | MainNav::Queue => {
                None
            }
        };
        // Artist page view data: bake album cover signals + lazily dispatch
        // their fetches (idempotent), mirroring how playlist rows resolve.
        let artist_data: Option<artist::ArtistViewData> = match nav {
            MainNav::Artist { .. } => state.library.open_artist.as_ref().map(|a| {
                let image = a
                    .image_url
                    .as_ref()
                    .and_then(|u| state.art.signal(&album_art::cache_key(u)));
                // Read existing signals only (immutable) — they were created
                // + dispatched in the reducer's `ArtistOpened` handler, off
                // this build's `home_art` borrow.
                let albums = a
                    .albums
                    .iter()
                    .map(|al| {
                        let cover = al
                            .image_url
                            .as_ref()
                            .and_then(|u| state.art.signal(&album_art::cache_key(u)));
                        artist::ArtistAlbumTile {
                            name: al.name.clone(),
                            year: al.release_date.chars().take(4).collect(),
                            cover,
                            id: al.id.clone(),
                        }
                    })
                    .collect();
                let popular = a
                    .top_tracks
                    .iter()
                    .map(|tk| {
                        let cover = tk
                            .album_image_url
                            .as_ref()
                            .and_then(|u| state.art.signal(&album_art::cache_key(u)));
                        artist::ArtistTrack {
                            title: tk.name.clone(),
                            cover,
                            duration: playlist::fmt_duration(tk.duration_ms),
                            uri: tk.uri.clone(),
                        }
                    })
                    .collect();
                artist::ArtistViewData {
                    name: a.name.clone(),
                    image,
                    followers: a.followers,
                    loading: a.loading,
                    popular,
                    albums,
                }
            }),
            _ => None,
        };
        let show_all_data: Option<show_all::ShowAllViewData> = match nav {
            MainNav::ShowAll { section } => Some(build_show_all(&state.art, home_ref, *section)),
            _ => None,
        };
        let now_playing = now_playing::NowPlaying {
            backdrop: &state.backdrop,
            player: &state.player_ui,
            canvas: &state.canvas,
            width: &state.prefs.now_playing_w,
        };
        let queue_ref = &state.library.queue;
        let player_bar = player_bar::PlayerBar {
            backdrop: &state.backdrop,
            player: &state.player_ui,
            on_action: self.on_action.clone(),
            devices: &state.devices,
            on_devices_open: self.on_devices_open.clone(),
            on_navigate: self.on_navigate.clone(),
            membership: &state.membership,
            on_like_open: self.on_like_open.clone(),
            icons,
        };
        let sidebar = sidebar::Sidebar {
            width: &state.prefs.sidebar_w,
            accent: &state.backdrop.accent,
            nav,
            on_navigate: self.on_navigate.clone(),
            home: home_ref,
            art: &state.art,
            icons,
        };
        let top_bar = top_bar::TopBar {
            settings: &state.settings.overlay,
            on_settings_open: self.on_settings_open.clone(),
            icons,
        };
        let main_pane = main_pane::MainPane {
            icons,
            home: home_ref,
            art: &state.art,
            accent: &state.backdrop.accent,
            nav,
            playlist: playlist.as_ref(),
            artist: artist_data.as_ref(),
            show_all: show_all_data.as_ref(),
            queue: queue_ref.as_deref(),
            pulse: &state.library.skeleton_pulse,
            on_skip: self.on_skip.clone(),
            on_context_menu: self.on_context_menu.clone(),
            main_t: &state.router.main_t,
            detail_collapse: &state.router.detail_collapse,
            on_play: self.on_play.clone(),
            on_navigate: self.on_navigate.clone(),
        };
        let settings_panel = settings::SettingsPanel {
            settings: &state.settings,
            canvas: &state.canvas,
            backdrop: &state.backdrop,
            profile: home_ref.profile.as_ref(),
            icons,
            sign_out: self.sign_out.clone(),
            on_canvas_change: self.on_canvas_change.clone(),
            on_clear_cache: self.on_clear_cache.clone(),
            on_change_cache_dir: self.on_change_cache_dir.clone(),
            quality: state.prefs.data.audio.quality,
            on_quality: self.on_quality.clone(),
            on_normalize: self.on_normalize.clone(),
        };
        let devices_panel = devices::DevicesPanel {
            devices: &state.devices,
            accent: &state.backdrop.accent,
            icons,
            on_transfer: self.on_transfer.clone(),
        };
        let like_menu = like_menu::LikeMenu {
            membership: &state.membership,
            liked: &state.player_ui.liked,
            accent: &state.backdrop.accent,
            icons,
            on_toggle_playlist: self.on_like_toggle_playlist.clone(),
            on_toggle_liked: self.on_like_toggle_liked.clone(),
        };
        let layout = Layout {
            backdrop_prev: &state.backdrop.prev,
            backdrop_curr: &state.backdrop.curr,
            crossfade_t: &state.backdrop.crossfade_t,
            art_luma: &state.backdrop.art_luma,
            sidebar_w: &state.prefs.sidebar_w,
            now_playing_w: &state.prefs.now_playing_w,
            mark_dirty: self.mark_dirty.clone(),
            now_playing: &now_playing,
            player_bar: &player_bar,
            sidebar: &sidebar,
            top_bar: &top_bar,
            main_pane: &main_pane,
            settings_panel: &settings_panel,
            devices_panel: &devices_panel,
            like_menu: &like_menu,
            menu: &state.menu,
            on_menu_add_queue: self.on_add_queue.clone(),
            on_menu_navigate: self.on_navigate.clone(),
            on_menu_close: self.on_menu_close.clone(),
        };
        render(s, &layout);
    }
}

/// Build a track payload for the picker's current target, taking the cover
/// and duration from the now-playing snapshot when it's still that track.
/// Used to live-patch an open playlist / Liked Songs page when the user
/// toggles membership, so the change shows without leaving the page.
pub(crate) fn target_track(state: &AppState) -> Option<crate::api::PlaylistTrack> {
    let target = &state.membership.target;
    if target.uri.is_empty() {
        return None;
    }
    let (album_image_url, duration_ms) = state
        .player_ui
        .snapshot
        .as_ref()
        .filter(|p| p.track_id == target.uri)
        .map(|p| (p.album_image_url.clone(), p.duration_ms))
        .unwrap_or((None, 0));
    Some(crate::api::PlaylistTrack {
        id: target.id.clone(),
        uri: target.uri.clone(),
        name: target.name.clone(),
        artist: target.artist.clone(),
        album: String::new(),
        album_image_url,
        duration_ms,
        artists: Vec::new(),
        album_id: String::new(),
        artist_id: String::new(),
        playable: true,
    })
}

/// Assemble a [`show_all::ShowAllViewData`] for `section` from the loaded
/// `HomeData`. Cover signals are read narrowly via `art.signal` (the prefetch
/// already created + dispatched them) — never `or_signal` here, so no
/// `home_art` borrow is held across the build. Recently-played is split into
/// day groups; the other sections are one ungrouped run.
fn build_show_all(
    art: &crate::model::ArtModel,
    home: &crate::api::HomeData,
    section: HomeSection,
) -> show_all::ShowAllViewData {
    use show_all::{ShowAllGroup, ShowAllRow, ShowAllViewData};
    let sig = |url: &Option<String>| {
        url.as_ref()
            .and_then(|u| art.signal(&album_art::cache_key(u)))
    };
    match section {
        HomeSection::Recent => {
            let (today, yesterday) = show_all::today_yesterday();
            let mut groups: Vec<ShowAllGroup> = Vec::new();
            for t in &home.recent {
                let label = show_all::day_label(&t.played_at, &today, &yesterday);
                // A song row plays the song (in its album context, so the
                // queue continues) — it doesn't navigate; opening the album
                // from a play-history list surprised more than it helped.
                let row = ShowAllRow {
                    title: t.name.clone(),
                    subtitle: t.artist.clone(),
                    thumb: sig(&t.album_image_url),
                    round: false,
                    action: show_all::RowAction::Play(crate::api::PlayTarget::ContextAt {
                        context_uri: format!("spotify:album:{}", t.album_id),
                        track_uri: format!("spotify:track:{}", t.id),
                    }),
                    menu: Some(crate::model::MenuTarget {
                        uri: format!("spotify:track:{}", t.id),
                        album_id: t.album_id.clone(),
                        artist_id: String::new(),
                    }),
                };
                if groups.last().map(|g| g.header.as_deref()) == Some(Some(label.as_str())) {
                    groups.last_mut().unwrap().rows.push(row);
                } else {
                    groups.push(ShowAllGroup {
                        header: Some(label),
                        rows: vec![row],
                    });
                }
            }
            ShowAllViewData {
                title: "Recently played".to_string(),
                groups,
            }
        }
        HomeSection::TopArtists => {
            let rows = home
                .top_artists
                .iter()
                .map(|a| ShowAllRow {
                    title: a.name.clone(),
                    subtitle: "Artist".to_string(),
                    thumb: sig(&a.image_url),
                    round: true,
                    action: show_all::RowAction::Open(MainNav::Artist { id: a.id.clone() }),
                    menu: None,
                })
                .collect();
            ShowAllViewData {
                title: "Your top artists".to_string(),
                groups: vec![ShowAllGroup { header: None, rows }],
            }
        }
        HomeSection::TopTracks => {
            let rows = home
                .top_tracks
                .iter()
                .map(|t| ShowAllRow {
                    title: t.name.clone(),
                    subtitle: t.artist.clone(),
                    thumb: sig(&t.album_image_url),
                    round: false,
                    // A song row plays (in its album context) — same as the
                    // recents rows and the home-feed song tiles.
                    action: show_all::RowAction::Play(crate::api::PlayTarget::ContextAt {
                        context_uri: format!("spotify:album:{}", t.album_id),
                        track_uri: format!("spotify:track:{}", t.id),
                    }),
                    menu: Some(crate::model::MenuTarget {
                        uri: format!("spotify:track:{}", t.id),
                        album_id: t.album_id.clone(),
                        artist_id: String::new(),
                    }),
                })
                .collect();
            ShowAllViewData {
                title: "Your top tracks".to_string(),
                groups: vec![ShowAllGroup { header: None, rows }],
            }
        }
        HomeSection::Playlists => {
            let rows = home
                .playlists
                .iter()
                .map(|p| ShowAllRow {
                    title: p.name.clone(),
                    subtitle: "Playlist".to_string(),
                    thumb: sig(&p.image_url),
                    round: false,
                    action: show_all::RowAction::Open(MainNav::Playlist {
                        id: p.id.clone(),
                        liked: false,
                    }),
                    menu: None,
                })
                .collect();
            ShowAllViewData {
                title: "Made For You".to_string(),
                groups: vec![ShowAllGroup { header: None, rows }],
            }
        }
    }
}

/// Switch the centre pane to `nav`. Ensures the target playlist is loaded
/// (TTL cache → fetch on miss/stale) via the library slice, flips the nav
/// state + entrance transition via the router, and requests the one scene
/// rebuild that swaps the pane content.
pub(crate) fn navigate(state: &mut AppState, cx: &mut Cx, worker: &Worker, nav: MainNav) {
    match &nav {
        MainNav::Playlist { id, liked } => {
            state.library.open_artist = None;
            let token = state.auth.token();
            state.library.open_for(&mut state.art, worker, token, id, *liked)
        }
        MainNav::Album { id } => {
            state.library.open_artist = None;
            let token = state.auth.token();
            state.library.open_album(&mut state.art, worker, token, id)
        }
        MainNav::Artist { id } => {
            state.library.open_playlist = None;
            let token = state.auth.token();
            state.library.open_artist(worker, token, id)
        }
        MainNav::ShowAll { .. } | MainNav::Home => {
            // Show-all renders from the already-loaded HomeData — no fetch.
            state.library.open_playlist = None;
            state.library.open_artist = None;
        }
        MainNav::Queue => {
            state.library.open_playlist = None;
            state.library.open_artist = None;
            // A remote device's queue arrives live off the cluster (full,
            // uncapped, auto-updating) — keep it. But when *Opal itself*
            // is the active player the cluster never echoes our queue, so a
            // previously-cached list goes stale (it won't show autoplay's
            // continuation): refetch from the Web API on every open. Also
            // fetch when nothing is loaded yet so the page resolves instead of
            // hanging on a skeleton.
            let self_play = state.devices.playing_on_self.get();
            if (self_play || state.library.queue.is_none())
                && let Some(token) = state.auth.token()
            {
                worker.fetch_queue(token);
            }
        }
    }
    state.router.go(nav, cx.tl, cx.now);
    cx.rebuild();
}
