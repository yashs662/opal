//! Now-playing pane — the first [`Component`].
//!
//! Reads its own model slices directly (`backdrop`/`player`/`canvas` +
//! the persisted pane width) instead of a wide prop bundle. Swaps between
//! a square album-art layout (with the live crossfade) and a full-bleed
//! Spotify-Canvas video layout depending on whether a clip is decoding.

use opal_gfx::{Computed, Justify, Len, Overflow, Scene, Signal};

use crate::model::{BackdropModel, CanvasModel, PlayerModel};
use crate::widgets::component::Component;
use crate::widgets::crossfade::crossfaded_art;
use crate::widgets::tokens as t;

pub struct NowPlaying<'a> {
    pub backdrop: &'a BackdropModel,
    pub player: &'a PlayerModel,
    pub canvas: &'a CanvasModel,
    /// Resizable pane width in logical px (driven by the right splitter).
    pub width: &'a Signal<f32>,
}

impl Component for NowPlaying<'_> {
    fn view(&self, s: &mut Scene) {
        // The decode thread flips `canvas.active` once video is flowing,
        // and a rebuild swaps this layout (see `CanvasModel::tick_active`).
        if self.canvas.active {
            self.canvas_layout(s);
        } else {
            self.art_layout(s);
        }
    }
}

impl NowPlaying<'_> {
    /// Default layout: square album art that crossfades on track change,
    /// title/artist below. The `now_playing_canvas` external node is kept
    /// here (transparent, over the art) so its `NodeId` always resolves —
    /// the decode thread needs a live target to push its first frame,
    /// which is what flips the layout to [`Self::canvas_layout`].
    fn art_layout(&self, s: &mut Scene) {
        s.col("now_playing")
            .width_px_bind(self.width.clone())
            .h(Len::Fill)
            .pad(t::SP_4)
            .gap(t::SP_3)
            .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.75)
            .radius(t::R_LG)
            .overflow_x(Overflow::Hidden)
            .child(|c| {
                c.text((), "Now playing", 16.0).color(t::TEXT);
                c.col(()).w(Len::Fill).square().child(|b| {
                    crossfaded_art(
                        b,
                        &self.backdrop.prev,
                        &self.backdrop.curr,
                        &self.backdrop.panel_t,
                        t::R_LG,
                    );
                    b.rect("now_playing_canvas")
                        .external()
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .radius(t::R_LG);
                });
                c.text_bound((), self.player.title.clone(), 14.0)
                    .color(t::TEXT)
                    .max_width_px(300.0);
                c.text_bound((), self.player.artist.clone(), 12.0)
                    .color(t::TEXT_DIM)
                    .max_width_px(300.0);
            });
    }

    /// Canvas-active layout: the video fills the pane width at its native
    /// 9:16 aspect as a full-bleed background, title/artist overlaid at the
    /// bottom over the video's own black→transparent edge fade. No padding
    /// so the video reaches the pane edges; corners clipped by the radius.
    fn canvas_layout(&self, s: &mut Scene) {
        s.col("now_playing")
            .width_px_bind(self.width.clone())
            .h(Len::Fill)
            .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.75)
            .radius(t::R_LG)
            .overflow(Overflow::Hidden, Overflow::Hidden)
            .child(|c| {
                // Video block: full pane width, 9:16 tall (Spotify Canvas).
                // The video alpha-fades over its bottom third so it
                // dissolves into the panel instead of a hard edge.
                c.col(())
                    .w(Len::Fill)
                    .aspect_ratio(9.0 / 16.0)
                    .on_hover(self.canvas.hover.clone())
                    .child(|b| {
                        b.rect("now_playing_canvas")
                            .external()
                            .radius(t::R_LG)
                            .fade_bottom(0.35)
                            .abs(0.0, 0.0)
                            .w(Len::Fill)
                            .h(Len::Fill);
                        // Dim overlay, painted *after* the external node so
                        // it composites on top of the video. A black
                        // gradient (solid top → transparent over the bottom
                        // 35%) matching the video's `fade_bottom`, tinted by
                        // `canvas.dim` (→ 0 on hover = full brightness).
                        if let Some(g) = self.canvas.dim_grad {
                            let dim = self.canvas.dim.clone();
                            b.image((), g)
                                .abs(0.0, 0.0)
                                .w(Len::Fill)
                                .h(Len::Fill)
                                .color(Computed::new((dim,), |(d,)| [1.0, 1.0, 1.0, d]));
                        }
                        // Title/artist over the faded lower area (no extra
                        // gradient — the video's own fade is the backdrop).
                        b.col(())
                            .abs(0.0, 0.0)
                            .w(Len::Fill)
                            .h(Len::Fill)
                            .justify(Justify::End)
                            .child(|ov| {
                                ov.col(())
                                    .w(Len::Fill)
                                    .pad_xy(t::SP_4, t::SP_5)
                                    .gap(t::SP_1)
                                    .child(|tx| {
                                        tx.text_bound((), self.player.title.clone(), 22.0)
                                            .color(t::TEXT)
                                            .max_width_px(300.0);
                                        tx.text_bound((), self.player.artist.clone(), 14.0)
                                            .color(t::TEXT)
                                            .max_width_px(300.0);
                                    });
                            });
                    });
            });
    }
}
