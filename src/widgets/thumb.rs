//! Fixed-size square cover thumbnail.

use opal_gfx::{ImageHandle, Len, Scene, Signal};

use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Renders the resolved cover when the signal carries `Some(handle)`
/// (overlaid on a dim placeholder so the pre-resolve frame doesn't pop).
/// `None` (no signal or unresolved) = placeholder only.
pub fn thumb(s: &mut Scene, art: Option<Signal<Option<ImageHandle>>>, size: f32, radius: f32) {
    s.col(()).w_px(size).h_px(size).child(|b| {
        // One node, never a placeholder rect stacked behind the cover: the
        // image paints its own rounded loading fill until it resolves, so a
        // loaded cover's anti-aliased corner can't reveal a layer behind it.
        match art {
            Some(sig) => {
                b.image_bound((), sig)
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .radius(radius)
                    .placeholder_fill(t::PLACEHOLDER);
            }
            None => {
                b.rect(())
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                    .radius(radius);
            }
        }
    });
}

/// The signature purple Liked-Songs tile — a gradient stand-in with a
/// centred heart, at whatever size the caller's list/hero needs. A rounded
/// `stack`, so the heart rides *inside* the group's single rounded corner
/// instead of the tile's corner anti-aliasing against a sibling.
pub fn liked_tile(s: &mut Scene, icons: &IconSet, size: f32, radius: f32, icon_px: f32) {
    s.stack(())
        .w_px(size)
        .h_px(size)
        .radius(radius)
        .rgba(0.36, 0.20, 0.78, 1.0)
        .center()
        .child(|b| {
            icons.render(b, Icon::Heart, icon_px, t::TEXT);
        });
}
