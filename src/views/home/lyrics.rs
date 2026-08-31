//! Lyrics page — the playing track's words, the line at the playhead lit.
//!
//! Every line is a wrapped text node whose colour is a [`Computed`] over
//! the model's active-line index, so the highlight moving from one line to
//! the next writes one signal and repaints; it never rebuilds the scene.
//! The frame loop scrolls the lit line to the middle (see
//! `app::frame::tick`), and clicking a line seeks the track to it.

use std::rc::Rc;

use opal_gfx::{Align, Bind, Computed, CursorIcon, Justify, Len, Lerp, Scene, Signal};

use crate::model::lyrics::{LyricsModel, LyricsStatus};
use crate::widgets::color::lift_for_chrome;
use crate::widgets::icon::IconSet;
use crate::widgets::tokens as t;

/// The page's scroller node. Stable (one lyrics page, whatever the track),
/// which is what lets the frame loop find it to scroll the active line in.
pub const SCROLL_NODE: &str = "lyrics_scroll";

/// Node name of the `i`-th lyric line — the frame loop resolves it to a
/// rect to centre the highlight.
pub fn line_node(i: usize) -> String {
    format!("lyric_line:{i}")
}

/// Resting line type size. Big and bold like the official client's page:
/// these are meant to be read across the room, not scanned like a track
/// list.
const LINE_TEXT: f32 = t::TEXT_3XL;
/// The line at the playhead is set larger — the emphasis is real type
/// size (the glyphs reshape), not a scaled raster, so it stays crisp.
const LINE_TEXT_LIT: f32 = t::TEXT_4XL;
/// Gap between lines, and the side gutter.
const LINE_GAP: f32 = t::SP_5;
/// Every line that isn't the one playing: far enough back that the lit
/// line reads from across the room, still legible for reading ahead.
const LINE_IDLE: [f32; 4] = [0.95, 0.95, 0.96, 0.30];
/// Saturation/brightness floors for the lit line. The lifted accent
/// clears the *chrome* contrast floor, which is not the same as reading
/// as a different colour from the dimmed lines around it — a washed-out
/// album accent lands close to their grey. These floors guarantee the
/// separation while leaving a genuinely vivid accent untouched.
const LINE_LIT_SAT: f32 = 0.55;
const LINE_LIT_VAL: f32 = 0.97;

/// Render the lyrics page. `on_seek_to(ms)` jumps playback to a clicked
/// line's timestamp; `accent` lights the line at the playhead.
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &IconSet,
    lyrics: &LyricsModel,
    accent: &Signal<[f32; 4]>,
    player: &crate::model::player::PlayerModel,
    on_seek_to: Rc<dyn Fn(u32)>,
    on_sync: Rc<dyn Fn()>,
    on_navigate: crate::views::home::NavFn,
) {
    // Stack: the scrolling words, with the sync pill floating over their
    // bottom edge. Square, so the group costs no compositor layer.
    s.stack("lyrics_page")
        .w(Len::Fill)
        .h(Len::Fill)
        .align(Align::Center)
        .justify(Justify::End)
        .child(|page| {
            lyric_scroller(page, lyrics, accent, player, &on_seek_to, &on_navigate);
            sync_pill(page, icons, lyrics, accent, on_sync);
        });
}

/// The scrolling column of lines (see [`SCROLL_NODE`]).
fn lyric_scroller(
    s: &mut Scene,
    lyrics: &LyricsModel,
    accent: &Signal<[f32; 4]>,
    player: &crate::model::player::PlayerModel,
    on_seek_to: &Rc<dyn Fn(u32)>,
    on_navigate: &crate::views::home::NavFn,
) {
    s.col(SCROLL_NODE)
        .w(Len::Fill)
        .h(Len::Fill)
        // Same SP_6 content inset as the queue/show-all pages.
        .pad_ltrb(t::SP_6, t::SP_6, t::SP_6, t::SP_6)
        .gap(LINE_GAP)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(|c| {
            header(c, player, on_navigate);
            match lyrics.status {
                LyricsStatus::Idle | LyricsStatus::Loading => {
                    message(c, "Loading lyrics…");
                }
                LyricsStatus::Unavailable => {
                    message(c, "No lyrics for this track");
                }
                LyricsStatus::Ready => lines(c, lyrics, accent, on_seek_to),
            }
        });
}

/// Track title + artist above the words — the page is reachable without
/// the now-playing pane open, so it names its own subject.
fn header(
    c: &mut Scene,
    player: &crate::model::player::PlayerModel,
    on_navigate: &crate::views::home::NavFn,
) {
    c.col(())
        .w(Len::Fill)
        .gap(t::SP_1)
        .pad_ltrb(t::SP_0, t::SP_0, t::SP_0, t::SP_4)
        .child(|h| {
            h.text_bound((), player.title.clone(), t::TEXT_LG)
                .color(t::TEXT);
            // Same clickable credit line every other page uses — each
            // artist opens their page (falls back to the joined names
            // while the credits are still resolving).
            let artists = player
                .with_snapshot(|p| p.artists.clone())
                .unwrap_or_default();
            let nav = on_navigate.clone();
            crate::widgets::artist_links::artist_links(
                h,
                "lyrics_artists",
                &artists,
                player.artist.clone(),
                t::TEXT_SM,
                t::TEXT_DIM.into(),
                Rc::new(move |ctx, id| {
                    nav(ctx, crate::views::MainNav::Artist { id: id.to_string() })
                }),
            );
        });
}

fn message(c: &mut Scene, text: &str) {
    c.row(()).w(Len::Fill).pad_xy(t::SP_0, t::SP_8).child(|r| {
        r.text((), text, t::TEXT_LG).color(t::TEXT_DIM);
    });
}

/// "Jump to current" — shown only once a manual scroll has dropped
/// auto-scroll. A frosted pill in the app's own chrome language (the same
/// glass + accent the collapsing headers and source pills use), riding its
/// own layer so the entry/exit (fade + rise) is composite-only: no
/// re-layout of the words behind it.
fn sync_pill(
    s: &mut Scene,
    icons: &IconSet,
    lyrics: &LyricsModel,
    accent: &Signal<[f32; 4]>,
    on_sync: Rc<dyn Fn()>,
) {
    let fg = crate::widgets::color::accent_fg(accent);
    let fg2 = fg.clone();
    s.glass(())
        .w(Len::Auto)
        .h_px(t::BTN_H_MD)
        // Lifted off the bottom edge; the stack's alignment centres it.
        .abs(0.0, -t::SP_6)
        .radius(t::R_FULL)
        .blur(14.0)
        .color(accent_wash(accent))
        .hover_color(t::HOVER_LIFT)
        .cursor(CursorIcon::Pointer)
        .layer_opacity(lyrics.pill_opacity.clone())
        .layer_offset_y(lyrics.pill_y.clone())
        .on_click(move |_| on_sync())
        // A glass node lays its children out in a column; the pill's
        // contents are one row, so they get their own.
        .child(move |p| {
            p.row(())
                .w(Len::Auto)
                .h(Len::Fill)
                .pad_xy(t::SP_4, t::SP_0)
                .gap(t::SP_2)
                .align(Align::Center)
                .child(move |r| {
                    // The glyph's ink sits higher in its box than the
                    // label's does in its line box, so centring the two
                    // rects leaves the icon reading high. One px down
                    // settles it optically.
                    r.col(())
                        .w_px(t::ICON_SM)
                        .h(Len::Fill)
                        .justify(Justify::Center)
                        .pad_ltrb(t::SP_0, 2.0, t::SP_0, t::SP_0)
                        .child(move |i| {
                            icons.render(
                                i,
                                crate::widgets::icon::Icon::Lyrics,
                                t::ICON_SM,
                                fg.clone(),
                            );
                        });
                    r.text((), "Follow along", t::TEXT_SM).color(fg2.clone());
                });
        });
}

/// The pill's fill: the album accent pulled down to a translucent wash, so
/// the frost reads as *this* track's chrome without shouting over the
/// words behind it.
fn accent_wash(accent: &Signal<[f32; 4]>) -> Computed<[f32; 4]> {
    Computed::new((accent.clone(),), |(a,)| [a[0], a[1], a[2], 0.30])
}

fn lines(
    c: &mut Scene,
    lyrics: &LyricsModel,
    accent: &Signal<[f32; 4]>,
    on_seek_to: &Rc<dyn Fn(u32)>,
) {
    let Some(l) = lyrics.lyrics.as_ref() else {
        return;
    };
    for (i, line) in l.lines.iter().enumerate() {
        // Spotify ships instrumental breaks as empty lines — keep them as
        // breathing room (the gap alone), with nothing to light or click.
        if line.text.trim().is_empty() {
            c.row(()).w(Len::Fill).h_px(t::SP_4);
            continue;
        }
        // Unsynced lyrics have no playhead to follow: every line rests at
        // full brightness (and one fixed size) instead of pretending one
        // of them is current.
        let (color, size): (Bind<[f32; 4]>, Bind<f32>) = if l.synced {
            // `emphasis` is 1 on the line at the playhead, 0 on every
            // other — but it *crosses* over the handover tween, so the
            // outgoing line shrinks and dims in step with the incoming
            // one growing and lighting.
            let emphasis = emphasis_for(i, lyrics);
            let e2 = emphasis.clone();
            let acc = accent.clone();
            (
                Computed::new((emphasis, acc), |(e, acc)| {
                    LINE_IDLE.lerp(lit_color(acc), e)
                })
                .into(),
                // Whole pixels only. A font size is a *raster* size — every
                // distinct value shapes and rasterizes its own glyphs, so a
                // continuously-varying one churns the glyph atlas (and
                // flickers as it evicts). Six integer steps reuse six cached
                // sizes across every line and every handover.
                Computed::new((e2,), |(e,)| {
                    (LINE_TEXT + (LINE_TEXT_LIT - LINE_TEXT) * e).round()
                })
                .into(),
            )
        } else {
            (t::TEXT.into(), LINE_TEXT.into())
        };
        let start_ms = line.start_ms;
        let seek = on_seek_to.clone();
        let mut row = c.row(line_node(i));
        row.w(Len::Fill).child(|r| {
            r.text((), &line.text, LINE_TEXT)
                .color(color)
                .font_size_bind(size)
                .w(Len::Fill)
                .wrap();
        });
        // Click a line to jump there — only meaningful when the timings
        // are real.
        if l.synced {
            row.cursor(CursorIcon::Pointer)
                .on_click(move |_| seek(start_ms));
        }
    }
    // The provider requires credit, and it also explains *why* a line is
    // occasionally off: these are Musixmatch's timings, not ours.
    if !l.provider.is_empty() {
        c.row(())
            .w(Len::Fill)
            .pad_ltrb(t::SP_0, t::SP_8, t::SP_0, t::SP_4)
            .child(|r| {
                r.text((), format!("Lyrics provided by {}", l.provider), t::TEXT_XS)
                    .color(t::TEXT_DIM);
            });
    }
    // Tail space so the last line can still scroll to the middle of the
    // viewport like every other one.
    c.row(()).w(Len::Fill).h_px(t::SP_40);
}

/// The lit line's colour: the accent, lifted to clear the chrome contrast
/// floor and then pushed toward white so it separates hard from
/// [`LINE_IDLE`] even for a dark or desaturated album.
fn lit_color(accent: [f32; 4]) -> [f32; 4] {
    let mut c = crate::widgets::color::vivid(lift_for_chrome(accent), LINE_LIT_SAT, LINE_LIT_VAL);
    c[3] = 1.0;
    c
}

/// How lit line `i` is, 0..=1: 1 while it's the line at the playhead, 0
/// otherwise — except across a handover, where the incoming line ramps up
/// and the outgoing one ramps down on the same tween. One shared tween
/// (`LyricsModel::handover`) drives every line, so a line change costs
/// three signal writes, not a rebuild.
fn emphasis_for(i: usize, lyrics: &LyricsModel) -> Computed<f32> {
    Computed::new(
        (
            lyrics.active.clone(),
            lyrics.prev.clone(),
            lyrics.handover.clone(),
        ),
        move |(active, prev, t)| {
            let t = t.clamp(0.0, 1.0);
            if active == i {
                t
            } else if prev == i {
                1.0 - t
            } else {
                0.0
            }
        },
    )
}
