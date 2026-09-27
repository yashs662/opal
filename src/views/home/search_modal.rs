//! Spotlight-style search modal — a top-anchored [`Overlay`] panel that
//! drops out of the search bar and springs open to fit its content
//! (the overlay's morph height, sprung on open/close). Empty shows the
//! recent-search history (clear one / all); typing shows Top result, Songs,
//! Artists, Albums, and Playlists, each opening its page (and recording into
//! history). The field autofocuses on open (`SceneCtx::request_focus`).

use std::rc::Rc;

use opal_gfx::{Align, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::SearchResults;
use crate::model::ArtModel;
use crate::model::MembershipModel;
use crate::model::search::{SearchHistoryEntry, SearchModel};
use crate::views::home::CtxMenuFn;
use crate::widgets::button::{TEXT_ACTION_PAD_X, text_action};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::thumb::thumb;
use crate::widgets::tokens as t;
use crate::widgets::track_row::{TrackRow, TrackRowActions};

/// Panel width (logical px) + the ceiling its spring height clamps to.
const MODAL_W: f32 = 620.0;
const MODAL_MAX_H: f32 = 640.0;
const INPUT_H: f32 = t::SP_12;
/// Inset of the scrolling content, and of every row inside it. Both sides
/// of the sum (`CONTENT_PAD + ROW_PAD_X`) land a thumb's left edge exactly
/// under the field's search glyph, so the panel reads as one column.
const CONTENT_PAD: f32 = t::SP_2;
const ROW_PAD_X: f32 = t::SP_2;
/// The field's own inset, kept equal to the rows' total inset so the search
/// glyph and the thumbs below it share one left edge.
const FIELD_PAD_X: f32 = CONTENT_PAD + ROW_PAD_X;
/// Height of a section header (its label, plus any trailing action).
const SECTION_H: f32 = t::SP_9;
/// Gap between the content column's children (headers + rows).
const CONTENT_GAP: f32 = t::SP_0_5;
/// Square hit box of a trailing icon button (the row's ×, the field's
/// clear), and the slack it carries around its glyph.
const ICON_BTN: f32 = t::SP_7;
/// Optical correction for that slack. A trailing control's *box* sits this
/// much further out than the row's nominal inset, so its glyph lands the
/// same distance from the panel edge as the thumbs on the left — otherwise
/// the right side reads 6 px airier than the left at every row.
const ICON_BTN_SLACK: f32 = (ICON_BTN - t::ICON_SM) / 2.0;
/// Trailing inset for a row that ends in an icon button, and for the
/// header, whose action pill carries its own padding instead.
const ROW_PAD_END: f32 = ROW_PAD_X - ICON_BTN_SLACK;
const HEADER_PAD_END: f32 = ROW_PAD_X - TEXT_ACTION_PAD_X;
const RESULT_ROW_H: f32 = t::SP_12;
const SONG_ROW_H: f32 = t::SP_14;
const TOP_RESULT_H: f32 = t::SP_16;
/// Per-section caps so the modal stays scannable.
const SECTION_CAP: usize = 4;
const SONGS_CAP: usize = 5;
const HISTORY_SHOWN: usize = 8;
/// Room left for an entry row's title/subtitle: the panel minus the
/// content + row insets, the thumb, the two gaps, and the trailing ×.
const ENTRY_TEXT_MAX: f32 =
    MODAL_W - 2.0 * CONTENT_PAD - ROW_PAD_X - ROW_PAD_END - t::THUMB_SM - 2.0 * t::SP_3 - ICON_BTN;

/// Clear-one/all history emitter.
type ClearFn = Rc<dyn Fn(Option<usize>)>;
/// A result/history entry was chosen (records + closes + opens/plays).
type SelectFn = Rc<dyn Fn(SearchHistoryEntry)>;

/// The field-only height the modal's overlay morphs *out from* on open (and
/// back to on close) — just the input row + its divider, so the field is
/// immediately visible (and focusable) while the content springs open below.
/// Handed to `Overlay::with_morph`.
pub fn collapsed_h() -> f32 {
    INPUT_H + t::SP_PX
}

/// Estimate the height the modal's current content wants — drives the morph
/// so it grows/shrinks as you type. Content scrolls beyond the clamp.
pub fn target_h(m: &SearchModel) -> f32 {
    // Accumulate the content column as (stacked child heights, child count):
    // its gap sits *between* children, so it can only be added once the count
    // is known — leaving it out clipped the last row against the panel edge.
    let mut content = 0.0_f32;
    let mut items = 0_usize;
    let mut push = |h: f32| {
        content += h;
        items += 1;
    };
    // A section = its header plus `n` rows of `row_h`.
    let mut section = |n: usize, cap: usize, row_h: f32| {
        if n > 0 {
            push(SECTION_H);
            for _ in 0..n.min(cap) {
                push(row_h);
            }
        }
    };
    if m.query.trim().is_empty() {
        // Empty history renders a single centred "No recent searches" row
        // (no section header); a populated one renders the header + rows.
        if m.history.is_empty() {
            push(t::SP_12);
        } else {
            section(m.history.len(), HISTORY_SHOWN, RESULT_ROW_H);
        }
    } else if let Some(r) = &m.results {
        let empty = r.tracks.is_empty()
            && r.artists.is_empty()
            && r.albums.is_empty()
            && r.playlists.is_empty();
        if empty {
            push(t::SP_12);
        } else {
            // Top result: the first artist, as its own one-row section.
            section(r.artists.len().min(1), 1, TOP_RESULT_H);
            section(r.tracks.len(), SONGS_CAP, SONG_ROW_H);
            section(r.artists.len().saturating_sub(1), SECTION_CAP, RESULT_ROW_H);
            section(r.albums.len(), SECTION_CAP, RESULT_ROW_H);
            section(r.playlists.len(), SECTION_CAP, RESULT_ROW_H);
        }
    } else {
        // Query typed, results not back yet — the "Searching…" row.
        push(t::SP_12);
    }
    // Field + divider + the content column's insets and gaps — the same
    // chrome the panel lays out, so the sprung height lands flush with the
    // last row instead of on a strip of dead space.
    let gaps = items.saturating_sub(1) as f32 * CONTENT_GAP;
    (collapsed_h() + 2.0 * CONTENT_PAD + content + gaps).min(MODAL_MAX_H)
}

pub struct SearchModal<'a> {
    pub icons: &'a Rc<IconSet>,
    pub art: &'a ArtModel,
    pub membership: &'a MembershipModel,
    pub search: &'a SearchModel,
    /// Field text changed.
    pub on_input: Rc<dyn Fn(String)>,
    /// A result / history entry was chosen (records + closes + opens/plays).
    pub on_select: SelectFn,
    /// Clear one recent search (`Some(i)`) or all (`None`).
    pub on_clear: ClearFn,
    /// Shared row affordances for the Songs list (heart + right-click menu).
    pub row_actions: &'a TrackRowActions,
    #[allow(dead_code)]
    pub on_context_menu: CtxMenuFn,
}

impl SearchModal<'_> {
    pub fn view(&self, s: &mut Scene) {
        let icons = self.icons;
        let art = self.art;
        let membership = self.membership;
        let m = self.search;
        let on_input = self.on_input.clone();
        let on_select = self.on_select.clone();
        let on_clear = self.on_clear.clone();
        let row_actions = self.row_actions;
        let panel_h = m.overlay.morph_height();
        self.search.overlay.render(s, t::SCRIM, move |host| {
            // Screen-centred (the overlay centres both axes). Growing the
            // height bind therefore expands the panel symmetrically from the
            // middle in every direction — no `.abs`, which would collapse the
            // centring host.
            host.col(())
                .w_px(MODAL_W)
                .height_px_bind(panel_h)
                .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
                .radius(t::R_XL)
                .border(1.0, t::BORDER)
                .clip()
                .child(move |panel| {
                    // Field row.
                    panel
                        .row(())
                        .w(Len::Fill)
                        .h_px(INPUT_H)
                        .pad_ltrb(FIELD_PAD_X, t::SP_0, FIELD_PAD_X - ICON_BTN_SLACK, t::SP_0)
                        .gap(t::SP_3)
                        .align(Align::Center)
                        .child(|f| {
                            icons.render(f, Icon::Search, t::ICON_MD, t::TEXT_DIM);
                            let on_change = on_input.clone();
                            f.text_field("search_input", &m.query, t::TEXT_BASE)
                                .w(Len::Fill)
                                .h(Len::Fill)
                                .align(Align::Center)
                                .justify(Justify::Start)
                                .placeholder("Search for songs, artists, albums, playlists")
                                .text_color(t::TEXT)
                                .placeholder_color(t::TEXT_DIM)
                                .on_change(move |s| on_change(s.to_string()));
                            if !m.query.is_empty() {
                                let clear_text = on_input.clone();
                                f.row(())
                                    .w_px(ICON_BTN)
                                    .h_px(ICON_BTN)
                                    .center()
                                    .radius(t::R_FULL)
                                    .hover_color(t::BTN_HOVER)
                                    .cursor(opal_gfx::CursorIcon::Pointer)
                                    .on_click(move |_| clear_text(String::new()))
                                    .child(|c| {
                                        icons.render(c, Icon::Close, t::ICON_SM, t::TEXT_DIM);
                                    });
                            }
                        });
                    panel.rect(()).w(Len::Fill).h_px(t::SP_PX).rgba(
                        t::BORDER[0],
                        t::BORDER[1],
                        t::BORDER[2],
                        t::BORDER[3],
                    );
                    // Scrolling content. `.layer()` (compositor scroll layer)
                    // is load-bearing: the rows' album column is a `marquee`,
                    // itself a composite layer — without a layer ancestor here
                    // the nested marquee layers hoist past this scroller and
                    // render outside the modal (escaping its clip + scroll).
                    panel
                        .col(())
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .pad(CONTENT_PAD)
                        .gap(CONTENT_GAP)
                        .scroll_y()
                        .layer()
                        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
                        .child(move |c| {
                            if m.query.trim().is_empty() {
                                history_section(c, art, icons, m, &on_select, &on_clear);
                            } else {
                                results_section(
                                    c,
                                    art,
                                    icons,
                                    membership,
                                    m.results.as_ref(),
                                    &on_select,
                                    row_actions,
                                );
                            }
                        });
                });
        });
    }
}

/// Section header: the label, plus an optional trailing action ("Clear
/// all"). Padded to the same inset as the rows underneath it, so the label
/// starts on the thumb column rather than floating left of it.
fn section_header(s: &mut Scene, title: &'static str, action: Option<(&'static str, ClearFn)>) {
    s.row(())
        .w(Len::Fill)
        .h_px(SECTION_H)
        .pad_ltrb(ROW_PAD_X, t::SP_0, HEADER_PAD_END, t::SP_0)
        .align(Align::Center)
        .justify(Justify::SpaceBetween)
        .child(move |r| {
            r.text((), title, t::TEXT_SM).color(t::TEXT_DIM);
            if let Some((label, clear)) = action {
                // The pill's own padding is what keeps its label clear of
                // the panel edge — it sits flush to the row inset.
                text_action(r, label, move |_| clear(None));
            }
        });
}

/// Reactive handle for an entry's cover. A pure read (the build must never
/// `or_signal` — it would `borrow_mut` the map mid-build), so every surface
/// feeding this view has to have dispatched its covers first: search results
/// in the worker reducer, restored history at boot (`main`). A URL with
/// no signal means someone added a path that skipped that — the row would
/// silently sit on a grey placeholder, so say so loudly in debug.
fn cover_of(art: &ArtModel, url: &Option<String>) -> Option<Signal<Option<ImageHandle>>> {
    let u = url.as_ref()?;
    let sig = art.signal(&crate::album_art::cache_key(u));
    if sig.is_none() {
        debug_assert!(false, "search cover with no signal: {u}");
        log::error!("search cover never dispatched (renders grey): {u}");
    }
    sig
}

fn history_section(
    s: &mut Scene,
    art: &ArtModel,
    icons: &Rc<IconSet>,
    m: &SearchModel,
    on_select: &SelectFn,
    on_clear: &ClearFn,
) {
    if m.history.is_empty() {
        s.row(()).w(Len::Fill).h(Len::Fill).center().child(|r| {
            r.text((), "No recent searches", t::TEXT_SM)
                .color(t::TEXT_DIM);
        });
        return;
    }
    section_header(s, "Recent searches", Some(("Clear all", on_clear.clone())));
    for (i, e) in m.history.iter().take(HISTORY_SHOWN).enumerate() {
        entry_row(
            s,
            art,
            icons,
            e.clone(),
            Some((i, on_clear.clone())),
            on_select,
        );
    }
}

/// One entry row (history or an artist/album/playlist result): thumb + name +
/// subtitle, opening on click. History rows carry a `×` (via `remove`).
fn entry_row(
    s: &mut Scene,
    art: &ArtModel,
    icons: &Rc<IconSet>,
    entry: SearchHistoryEntry,
    remove: Option<(usize, ClearFn)>,
    on_select: &SelectFn,
) {
    let round = entry.kind == "artist";
    let radius = if round { t::R_FULL } else { t::R_SM };
    let cover = cover_of(art, &entry.image_url);
    let sel = on_select.clone();
    let click_entry = entry.clone();
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(RESULT_ROW_H)
        .pad_ltrb(ROW_PAD_X, t::SP_1, ROW_PAD_END, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .cursor(opal_gfx::CursorIcon::Pointer)
        .on_click(move |_| sel(click_entry.clone()));
    row.child(move |r| {
        thumb(r, cover.clone(), t::THUMB_SM, radius);
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|mid| {
                mid.text((), &entry.name, t::TEXT_BASE)
                    .color(t::TEXT)
                    .max_width_px(ENTRY_TEXT_MAX);
                mid.text((), &entry.subtitle, t::TEXT_XS)
                    .color(t::TEXT_DIM)
                    .max_width_px(ENTRY_TEXT_MAX);
            });
        if let Some((idx, clear)) = remove {
            r.row(())
                .push_end()
                .w_px(ICON_BTN)
                .h_px(ICON_BTN)
                .center()
                .radius(t::R_FULL)
                .hover_color(t::BTN_HOVER)
                .cursor(opal_gfx::CursorIcon::Pointer)
                .on_click(move |_| clear(Some(idx)))
                .child(|c| {
                    icons.render(c, Icon::Close, t::ICON_SM, t::TEXT_DIM);
                });
        }
    });
}

fn results_section(
    s: &mut Scene,
    art: &ArtModel,
    icons: &Rc<IconSet>,
    membership: &MembershipModel,
    results: Option<&SearchResults>,
    on_select: &SelectFn,
    row_actions: &TrackRowActions,
) {
    let Some(r) = results else {
        s.row(()).w(Len::Fill).h(Len::Fill).center().child(|x| {
            x.text((), "Searching\u{2026}", t::TEXT_SM)
                .color(t::TEXT_DIM);
        });
        return;
    };
    if r.tracks.is_empty() && r.artists.is_empty() && r.albums.is_empty() && r.playlists.is_empty()
    {
        s.row(()).w(Len::Fill).h(Len::Fill).center().child(|x| {
            x.text((), "No results", t::TEXT_SM).color(t::TEXT_DIM);
        });
        return;
    }
    // Top result — the first artist, as a prominent card.
    if let Some(a) = r.artists.first() {
        section_header(s, "Top result", None);
        let cover = cover_of(art, &a.image_url);
        let entry = artist_entry(a);
        let sel = on_select.clone();
        let click_entry = entry.clone();
        s.row(())
            .w(Len::Fill)
            .h_px(TOP_RESULT_H)
            .pad(t::SP_2)
            .gap(t::SP_3)
            .align(Align::Center)
            .radius(t::R_LG)
            .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
            .hover_color(t::BTN_HOVER)
            .cursor(opal_gfx::CursorIcon::Pointer)
            .on_click(move |_| sel(click_entry.clone()))
            .child(move |row| {
                thumb(row, cover.clone(), t::SP_12, t::R_FULL);
                row.col(()).gap(t::SP_0_5).child(|m| {
                    m.text((), &entry.name, t::TEXT_LG)
                        .color(t::TEXT)
                        .max_width_px(320.0);
                    m.text((), "Artist", t::TEXT_XS).color(t::TEXT_DIM);
                });
            });
    }
    // Songs — the shared row (heart + right-click menu); click plays.
    if !r.tracks.is_empty() {
        section_header(s, "Songs", None);
        for tk in r.tracks.iter().take(SONGS_CAP) {
            let cover = cover_of(art, &tk.album_image_url);
            let sel = on_select.clone();
            let entry = SearchHistoryEntry {
                kind: "track".into(),
                id: tk.id.clone(),
                name: tk.name.clone(),
                subtitle: tk.artist.clone(),
                image_url: tk.album_image_url.clone(),
            };
            crate::widgets::track_row::track_row(
                s,
                TrackRow {
                    index: None,
                    track: tk.clone(),
                    cover,
                    duration: crate::views::home::playlist::fmt_duration(tk.duration_ms),
                    activate: Rc::new(move || sel(entry.clone())),
                    sources: None,
                    in_library: membership.is_saved(&tk.uri),
                    playable: tk.playable,
                    plays: None,
                },
                row_actions,
            );
        }
    }
    // Artists (past the top result) / Albums / Playlists as compact rows.
    if r.artists.len() > 1 {
        section_header(s, "Artists", None);
        for a in r.artists.iter().skip(1).take(SECTION_CAP) {
            entry_row(s, art, icons, artist_entry(a), None, on_select);
        }
    }
    if !r.albums.is_empty() {
        section_header(s, "Albums", None);
        for a in r.albums.iter().take(SECTION_CAP) {
            let entry = SearchHistoryEntry {
                kind: "album".into(),
                id: a.id.clone(),
                name: a.name.clone(),
                subtitle: a.artist.clone(),
                image_url: a.image_url.clone(),
            };
            entry_row(s, art, icons, entry, None, on_select);
        }
    }
    if !r.playlists.is_empty() {
        section_header(s, "Playlists", None);
        for p in r.playlists.iter().take(SECTION_CAP) {
            let entry = SearchHistoryEntry {
                kind: "playlist".into(),
                id: p.id.clone(),
                name: p.name.clone(),
                subtitle: "Playlist".into(),
                image_url: p.image_url.clone(),
            };
            entry_row(s, art, icons, entry, None, on_select);
        }
    }
}

fn artist_entry(a: &crate::api::ArtistRef) -> SearchHistoryEntry {
    SearchHistoryEntry {
        kind: "artist".into(),
        id: a.id.clone(),
        name: a.name.clone(),
        subtitle: "Artist".into(),
        image_url: a.image_url.clone(),
    }
}
