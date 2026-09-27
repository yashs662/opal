//! Reusable pill button — the bordered, hover-filling, rounded button
//! used for account actions (the settings "Sign out") and the login
//! screen's Back / Reset controls.
//!
//! Auto-sizes to its content (icon + label) so call sites don't hand-pick
//! a width. Two tones: [`ButtonTone::Neutral`] (the default account-action
//! look) and [`ButtonTone::Danger`] (a red scheme for destructive actions
//! like resetting preferences).
//!
//! [`text_action`] is the borderless sibling — the small dim label that
//! trails a section header ("Clear all" / "Show all"), with a hit target
//! and hover fill instead of bare text.

use opal_gfx::{Align, EventCtx, Len, Scene};

use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Visual scheme for a [`pill_button`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonTone {
    /// Neutral outline — white text/border, subtle hover fill.
    Neutral,
    /// Destructive — red text/border, red hover fill.
    Danger,
}

impl ButtonTone {
    /// `(text+icon colour, border colour, hover-fill colour)`.
    fn colors(self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        match self {
            ButtonTone::Neutral => (t::TEXT, t::BORDER, t::BTN_HOVER),
            ButtonTone::Danger => (
                [0.93, 0.46, 0.46, 1.0],
                [0.93, 0.46, 0.46, 0.45],
                [0.85, 0.20, 0.20, 0.18],
            ),
        }
    }
}

/// Build a pill button with an optional leading `icon` and a text `label`.
/// Auto-sized (content + horizontal padding); chain nothing else — pass the
/// click handler in. Mirrors the settings "Sign out" button styling.
pub fn pill_button(
    s: &mut Scene,
    icons: &IconSet,
    label: &str,
    icon: Option<Icon>,
    tone: ButtonTone,
    on_click: impl Fn(&mut EventCtx) + 'static,
) {
    let (fg, border, hover) = tone.colors();
    s.row(())
        .w(Len::Auto)
        .h_px(t::SP_9)
        .pad_xy(t::SP_4, 0.0)
        .gap(t::SP_2)
        .align(Align::Center)
        .radius(t::R_FULL)
        .border(1.0, border)
        .hover_color(hover)
        .on_click(on_click)
        .child(|b| {
            if let Some(ic) = icon {
                icons.render(b, ic, t::ICON_SM, fg);
            }
            b.text((), label, t::TEXT_SM).color(fg);
        });
}

/// Horizontal padding inside a [`text_action`]. Exposed so a header can
/// pull its own right inset in by the same amount and land the *label* on
/// the content edge, not the pill's box.
pub const TEXT_ACTION_PAD_X: f32 = t::SP_2;

/// Borderless inline action beside a section header — dim label that
/// brightens and picks up a subtle fill on hover. Auto-sized, with real
/// padding so the hit target is a pill rather than the glyphs themselves.
pub fn text_action(s: &mut Scene, label: &str, on_click: impl Fn(&mut EventCtx) + 'static) {
    s.row(())
        .w(Len::Auto)
        .h_px(t::BTN_H_SM)
        .pad_xy(TEXT_ACTION_PAD_X, 0.0)
        .align(Align::Center)
        .radius(t::R_FULL)
        .hover_color(t::BTN_HOVER)
        .cursor(opal_gfx::CursorIcon::Pointer)
        .on_click(on_click)
        .child(|b| {
            b.text((), label, t::TEXT_XS)
                .color(t::TEXT_DIM)
                .hover_color(t::TEXT);
        });
}
