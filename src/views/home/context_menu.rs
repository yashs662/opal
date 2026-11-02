//! Right-click context menu — one menu for every right-click surface.
//!
//! Hosted by the menu's [`Overlay`] (transparent scrim): any press outside
//! the box dismisses it, and it stacks by open order like every other
//! popup. The box is anchored at the cursor; its items depend on what was
//! right-clicked ([`MenuKind`]):
//! - a track row: Add to queue (works on any device, remote included), Go
//!   to song radio, Go to album / Go to artist when those ids are known, and
//!   Add to playlist…;
//! - the window's close button (lossless engine running): close and keep
//!   Spotify playing, or close as usual.

use std::rc::Rc;

use opal_gfx::{Align, EventCtx, Len, Overlay, Scene, WindowAction};

use crate::model::{MenuKind, MenuModel, MenuTarget};
use crate::views::MainNav;
use crate::views::home::{LikeForFn, NavFn};
use crate::widgets::tokens as t;

/// Menu width (logical px).
const MENU_W: f32 = 220.0;

/// The menu's action emitters, built once by the Home view — clone-cheap.
#[derive(Clone)]
pub struct MenuActions {
    /// Enqueue a track uri.
    pub add_queue: Rc<dyn Fn(String)>,
    /// Open an album / artist page.
    pub navigate: NavFn,
    /// Open the like picker targeted at a track.
    pub add_playlist: LikeForFn,
    /// Open a track's song radio.
    pub song_radio: LikeForFn,
    /// Close Opal, leaving the lossless engine playing.
    pub quit_keep_engine: Rc<dyn Fn()>,
}

/// Render the context menu (skipped by the overlay while closed). Every
/// item closes the menu after acting.
pub fn view(s: &mut Scene, menu: &MenuModel, actions: &MenuActions) {
    let overlay = menu.overlay.clone();
    let kind = menu.kind.clone();
    let actions = actions.clone();
    // Opaque box — a context menu should read as solid chrome, not let the
    // row bleed through. `PANEL_HI` is a touch lighter than the pane so it
    // lifts off the content.
    menu.overlay.render_at(s, [0.0; 4], menu.pos, move |host| {
        host.col(())
            .w_px(MENU_W)
            .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
            .radius(t::R_MD)
            .border(1.0, t::BORDER)
            .pad(t::SP_1)
            .gap(t::SP_0_5)
            .child(move |m| match kind {
                MenuKind::Track(target) => track_items(m, &overlay, target, actions),
                MenuKind::Window => window_items(m, &overlay, actions),
            });
    });
}

/// Track-row actions.
fn track_items(m: &mut Scene, overlay: &Overlay, target: MenuTarget, a: MenuActions) {
    let uri = target.uri.clone();
    let add_queue = a.add_queue;
    item(m, overlay, "Add to queue", move |_| add_queue(uri.clone()));
    // A station seeded on this track.
    if let Some(track) = target.track.clone() {
        let radio = a.song_radio;
        item(m, overlay, "Go to song radio", move |ctx| {
            radio(ctx, (*track).clone())
        });
    }
    let album = (!target.album_id.is_empty()).then(|| MainNav::Album {
        id: target.album_id.clone(),
    });
    let artist = (!target.artist_id.is_empty()).then(|| MainNav::Artist {
        id: target.artist_id.clone(),
    });
    for (label, nav) in [("Go to album", album), ("Go to artist", artist)] {
        if let Some(nav) = nav {
            let go = a.navigate.clone();
            item(m, overlay, label, move |ctx| go(ctx, nav.clone()));
        }
    }
    // The like picker, targeted at this row.
    if let Some(track) = target.track {
        let add = a.add_playlist;
        item(m, overlay, "Add to playlist\u{2026}", move |ctx| {
            add(ctx, (*track).clone())
        });
    }
}

/// Close-button actions while the lossless engine runs.
fn window_items(m: &mut Scene, overlay: &Overlay, a: MenuActions) {
    let keep = a.quit_keep_engine;
    item(m, overlay, "Close, keep Spotify playing", move |_| keep());
    item(m, overlay, "Close", |ctx| {
        ctx.tree.request_window_action(WindowAction::Close)
    });
}

/// One menu row — a hover-highlighted label that runs `act`, then closes
/// the menu. A popup the action opens (the like picker) stamps a newer
/// stack key on open, so it lands above the menu as the menu fades out.
fn item(s: &mut Scene, overlay: &Overlay, label: &str, act: impl Fn(&mut EventCtx) + 'static) {
    let overlay = overlay.clone();
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_9)
        .pad_xy(t::SP_3, t::SP_0)
        .align(Align::Center)
        .radius(t::R_SM)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .on_click(move |ctx| {
            act(ctx);
            overlay.close(ctx.timeline, ctx.now);
        })
        .child(|r| {
            r.text((), label, t::TEXT_SM).color(t::TEXT);
        });
}
