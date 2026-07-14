//! Pill-shaped content filter chip.

use opal_gfx::{Scene, Signal};

use crate::widgets::color::accent_fg;
use crate::widgets::tokens as t;

/// Selected chip uses the live accent colour (derived from the current
/// album art) on a contrasting text foreground — pulls the album palette
/// into the chrome the same way the play pill does. Unselected chips sit
/// on the panel-highlight colour.
pub fn chip(s: &mut Scene, label: &str, selected: bool, accent: &Signal<[f32; 4]>) {
    let mut row = s.row(());
    row.h_px(t::CHIP_H)
        .pad_xy(t::SP_3_5, t::SP_0)
        .center()
        .radius(t::R_FULL);
    if selected {
        row.color(accent.clone()).hover_opacity(0.9).child(|c| {
            c.text((), label, 13.0).color(accent_fg(accent));
        });
    } else {
        row.color(t::PANEL_HI).hover_opacity(0.8).child(|c| {
            c.text((), label, 13.0).color(t::TEXT);
        });
    }
}
