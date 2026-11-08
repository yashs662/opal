//! Pill-shaped chip — content filters (sidebar) and option pickers
//! (settings rows).

use opal_gfx::{NodeBuilderRef, Scene, Signal};

use crate::widgets::color::{AccentSurface, accent_surface};
use crate::widgets::tokens as t;

/// A chip's look.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ChipState {
    /// The live accent (from the current album art) on a contrasting
    /// foreground — pulls the album palette into the chrome the way the play
    /// pill does.
    Selected,
    /// Panel-highlight fill that dims under the cursor.
    Idle,
    /// Visibly inert: greyed, no hover — for a choice that governs nothing
    /// right now.
    Disabled,
}

/// Build a chip. Returns its builder so the caller can wire a click.
pub fn chip<'s>(
    s: &'s mut Scene,
    label: &str,
    state: ChipState,
    accent: &Signal<[f32; 4]>,
) -> NodeBuilderRef<'s> {
    let mut row = s.row(());
    row.h_px(t::CHIP_H)
        .pad_xy(t::SP_3_5, t::SP_0)
        .center()
        .radius(t::R_FULL);
    match state {
        ChipState::Selected => {
            let c = accent_surface(accent, AccentSurface::Solid);
            row.color(c.fill).child(|r| {
                r.text((), label, t::TEXT_SM).color(c.fg);
            });
        }
        ChipState::Idle => {
            row.color(t::PANEL_HI).hover_opacity(0.8).child(|r| {
                r.text((), label, t::TEXT_SM).color(t::TEXT);
            });
        }
        ChipState::Disabled => {
            row.color(t::PANEL_HI).opacity(0.45).child(|r| {
                r.text((), label, t::TEXT_SM).color(t::TEXT_DIM);
            });
        }
    }
    row
}
