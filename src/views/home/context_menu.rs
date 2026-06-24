//! Right-click context menu for track rows.
//!
//! Rendered last in the Home scene (on top of everything). When open, a
//! full-window transparent scrim captures the next click/right-click to
//! dismiss, and a small menu box is anchored at the cursor with the
//! track's actions: Add to queue (works on any device, remote included),
//! and Go to album / Go to artist when those ids are known.

use std::rc::Rc;

use opal_gfx::{Align, Len, Scene};

use crate::model::MenuModel;
use crate::views::MainNav;
use crate::views::home::NavFn;
use crate::widgets::tokens as t;

/// Menu width (logical px).
const MENU_W: f32 = 200.0;

/// Render the context menu if open. `on_add_queue(uri)` enqueues the
/// track; `on_navigate` opens album/artist; `on_close` dismisses (both
/// the scrim and every action close it).
pub fn view(
    s: &mut Scene,
    menu: &MenuModel,
    on_add_queue: Rc<dyn Fn(String)>,
    on_navigate: NavFn,
    on_close: Rc<dyn Fn()>,
) {
    if !menu.open {
        return;
    }
    let pos = menu.pos;
    let target = menu.target.clone();

    // Full-window scrim: transparent but click/right-click-absorbing, so
    // the next press anywhere outside the menu dismisses it.
    let close_scrim = on_close.clone();
    let close_scrim_r = on_close.clone();
    s.rect(())
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        .on_click(move |_| close_scrim())
        .on_right_click(move |_| close_scrim_r());

    // Menu box anchored at the cursor — opaque (a context menu should
    // read as solid chrome, not let the row bleed through). `PANEL_HI` is
    // a touch lighter than the pane so it lifts off the content.
    s.col(())
        .abs(pos[0], pos[1])
        .w_px(MENU_W)
        .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
        .radius(t::R_MD)
        .border(1.0, t::BORDER)
        .pad(t::SP_1)
        .gap(t::SP_0_5)
        .child(move |m| {
            // Add to queue.
            let uri = target.uri.clone();
            let add = on_add_queue.clone();
            let close = on_close.clone();
            item(m, "Add to queue", move |_| {
                add(uri.clone());
                close();
            });
            // Go to album.
            if !target.album_id.is_empty() {
                let nav = on_navigate.clone();
                let id = target.album_id.clone();
                let close = on_close.clone();
                item(m, "Go to album", move |ctx| {
                    nav(ctx, MainNav::Album { id: id.clone() });
                    close();
                });
            }
            // Go to artist.
            if !target.artist_id.is_empty() {
                let nav = on_navigate.clone();
                let id = target.artist_id.clone();
                let close = on_close.clone();
                item(m, "Go to artist", move |ctx| {
                    nav(ctx, MainNav::Artist { id: id.clone() });
                    close();
                });
            }
        });
}

/// One menu row — a hover-highlighted label with a click action.
fn item(s: &mut Scene, label: &str, on_click: impl Fn(&mut opal_gfx::EventCtx) + 'static) {
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_9)
        .pad_xy(t::SP_3, t::SP_0)
        .align(Align::Center)
        .radius(t::R_SM)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .on_click(on_click)
        .child(|r| {
            r.text((), label, t::TEXT_SM).color(t::TEXT);
        });
}
