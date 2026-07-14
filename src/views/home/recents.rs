//! Spotify-style "Recents" page — the recently-played show-all, session-
//! grouped. Within each day, consecutive plays from the same context (a
//! playlist / album / Liked Songs / …) collapse into one row —
//! "Gud ol songs · 44 songs played · Playlist · Yash Sharma" — that expands
//! to the individual tracks. No filter pills.
//!
//! The aggregation + context resolution happen in `mod.rs`
//! (`build_recents`); this module is pure rendering. The main row opens the
//! context's detail page; the trailing chevron toggles the expander.

use std::rc::Rc;

use opal_gfx::{Align, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlayTarget;
use crate::model::MenuTarget;
use crate::views::MainNav;
use crate::views::home::{CtxMenuFn, NavFn, PlayFn};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

const SESSION_H: f32 = t::SP_14;
const TRACK_H: f32 = t::SP_12;

/// One track inside an expanded session.
pub struct RecentTrackRow {
    pub title: String,
    pub subtitle: String,
    pub thumb: Option<Signal<Option<ImageHandle>>>,
    pub play: PlayTarget,
    pub menu: MenuTarget,
}

/// A session — a run of consecutive plays sharing one context.
pub struct RecentSession {
    /// Stable key for the expand state (survives rebuild).
    pub key: String,
    pub title: String,
    /// "" hides the subtitle (contextless "N songs played").
    pub subtitle: String,
    pub thumb: Option<Signal<Option<ImageHandle>>>,
    /// Circular thumb for an artist context.
    pub round: bool,
    pub expanded: bool,
    /// The context's detail page — the main row opens it; `None` for a
    /// contextless run (the whole row just toggles the expander).
    pub open: Option<MainNav>,
    pub tracks: Vec<RecentTrackRow>,
}

pub struct RecentDay {
    pub label: String,
    pub sessions: Vec<RecentSession>,
}

pub struct RecentsViewData {
    pub title: String,
    pub days: Vec<RecentDay>,
}

/// Render the Recents page into `s` (the caller's transition wrapper).
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &RecentsViewData,
    scroll_node: &str,
    on_navigate: NavFn,
    on_play: PlayFn,
    on_context_menu: CtxMenuFn,
    on_toggle: Rc<dyn Fn(String)>,
) {
    s.col(scroll_node)
        .w(Len::Fill)
        .h(Len::Fill)
        .pad_ltrb(t::SP_6, t::SP_2, t::SP_6, t::SP_6)
        .gap(t::SP_3)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
            c.text((), &data.title, 28.0)
                .color(t::TEXT)
                .max_width_px(520.0);
            for day in &data.days {
                c.row(())
                    .w(Len::Fill)
                    .h_px(t::SP_8)
                    .align(Align::End)
                    .child(|r| {
                        r.text((), &day.label, 16.0).color(t::TEXT);
                    });
                for sess in &day.sessions {
                    session(
                        c,
                        icons,
                        sess,
                        &on_navigate,
                        &on_play,
                        &on_context_menu,
                        &on_toggle,
                    );
                    if sess.expanded {
                        for tr in &sess.tracks {
                            track(c, tr, &on_play, &on_context_menu);
                        }
                    }
                }
            }
        });
}

/// The collapsible session header row.
#[allow(clippy::too_many_arguments)]
fn session(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    sess: &RecentSession,
    nav: &NavFn,
    _play: &PlayFn,
    _menu: &CtxMenuFn,
    on_toggle: &Rc<dyn Fn(String)>,
) {
    let radius = if sess.round { t::R_FULL } else { t::R_SM };
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(SESSION_H)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .cursor(opal_gfx::CursorIcon::Pointer);
    // Main area opens the context's page; contextless runs just toggle.
    match &sess.open {
        Some(target) => {
            let nav = nav.clone();
            let target = target.clone();
            row.on_click(move |ctx| nav(ctx, target.clone()));
        }
        None => {
            let toggle = on_toggle.clone();
            let key = sess.key.clone();
            row.on_click(move |_| toggle(key.clone()));
        }
    }
    row.child(|r| {
        r.col(()).w_px(t::THUMB_MD).h_px(t::THUMB_MD).child(|b| {
            if let Some(sig) = sess.thumb.clone() {
                b.image_bound((), sig)
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .radius(radius)
                    .placeholder_fill(t::PLACEHOLDER);
            } else {
                b.rect(())
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                    .radius(radius);
            }
        });
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), &sess.title, 14.0)
                    .color(t::TEXT)
                    .max_width_px(420.0);
                if !sess.subtitle.is_empty() {
                    m.text((), &sess.subtitle, 12.0)
                        .color(t::TEXT_DIM)
                        .max_width_px(420.0);
                }
            });
        // Trailing expander — its own click target so it toggles even when
        // the main area navigates to the context page.
        let toggle = on_toggle.clone();
        let key = sess.key.clone();
        let glyph = if sess.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        r.row(())
            .push_end()
            .w_px(t::SP_8)
            .h_px(t::SP_8)
            .center()
            .radius(t::R_FULL)
            .hover_color(t::BTN_HOVER)
            .cursor(opal_gfx::CursorIcon::Pointer)
            .on_click(move |_| toggle(key.clone()))
            .child(|c| {
                icons.render(c, glyph, t::ICON_MD, t::TEXT_DIM);
            });
    });
}

/// One expanded track row — plays the track; full right-click menu.
fn track(s: &mut Scene, tr: &RecentTrackRow, play: &PlayFn, on_context_menu: &CtxMenuFn) {
    let play = play.clone();
    let target = tr.play.clone();
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(TRACK_H)
        // Inset past the session thumb so the tracks read as nested under it.
        .pad_ltrb(t::SP_12, t::SP_1, t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .cursor(opal_gfx::CursorIcon::Pointer)
        .on_click(move |_| play(target.clone()));
    crate::views::home::attach_context_menu(&mut row, on_context_menu, tr.menu.clone());
    row.child(|r| {
        r.col(()).w_px(t::THUMB_SM).h_px(t::THUMB_SM).child(|b| {
            if let Some(sig) = tr.thumb.clone() {
                b.image_bound((), sig)
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .radius(t::R_SM)
                    .placeholder_fill(t::PLACEHOLDER);
            } else {
                b.rect(())
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                    .radius(t::R_SM);
            }
        });
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), &tr.title, 14.0)
                    .color(t::TEXT)
                    .max_width_px(420.0);
                m.text((), &tr.subtitle, 12.0)
                    .color(t::TEXT_DIM)
                    .max_width_px(420.0);
            });
    });
}
