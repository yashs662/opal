//! Full-width "Show all" list page (Spotify "Recents"-style) rendered into
//! the centre pane. Expands a home-feed section into a vertical list of
//! full-width rows — thumb + title + subtitle + chevron — optionally split
//! into day-labelled groups (Recently played). Built entirely from the
//! already-loaded `HomeData`; each row opens its detail page.
//!
//! Lightweight (≤ ~20 rows), so it's a plain `scroll_y` column — no
//! virtualised list, no collapsing header.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use opal_gfx::{Align, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlayTarget;
use crate::model::MenuTarget;
use crate::views::MainNav;
use crate::views::home::{CtxMenuFn, NavFn, PlayFn};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Full-width row height.
const ROW_H: f32 = t::SP_14;

/// What clicking a row does. Container rows (playlist/artist/album) open
/// their detail page; song rows (Recently played) just play the song —
/// in its album context, so the queue continues naturally.
pub enum RowAction {
    Open(MainNav),
    Play(PlayTarget),
}

/// One full-width list row.
pub struct ShowAllRow {
    pub title: String,
    pub subtitle: String,
    pub thumb: Option<Signal<Option<ImageHandle>>>,
    /// Circular thumb (artists) vs rounded square (everything else).
    pub round: bool,
    pub action: RowAction,
    /// Right-click target for track rows (Add to queue / Go to album).
    /// `None` for container rows (playlist/artist) — they have no track to
    /// enqueue.
    pub menu: Option<MenuTarget>,
}

/// A run of rows under an optional header (day label for Recently played;
/// `None` for the ungrouped sections).
pub struct ShowAllGroup {
    pub header: Option<String>,
    pub rows: Vec<ShowAllRow>,
}

/// Everything the page needs for one render.
pub struct ShowAllViewData {
    pub title: String,
    pub groups: Vec<ShowAllGroup>,
}

/// Render the Show-all page into `s` (the caller's transition wrapper).
/// `scroll_node` is the content-scoped scroller name (rebuilds preserve
/// scroll by identity; a different section ⇒ different name ⇒ fresh top).
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &ShowAllViewData,
    scroll_node: &str,
    on_navigate: NavFn,
    on_play: PlayFn,
    on_context_menu: CtxMenuFn,
) {
    s.col(scroll_node)
        .w(Len::Fill)
        .h(Len::Fill)
        // Bottom inset matches the sides (see the queue scroller); back
        // navigation lives in the top-bar history arrows.
        .pad_ltrb(t::SP_6, t::SP_2, t::SP_6, t::SP_6)
        .gap(t::SP_3)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
            c.text((), &data.title, 28.0)
                .color(t::TEXT)
                .max_width_px(520.0);

            for group in &data.groups {
                if let Some(h) = &group.header {
                    c.row(())
                        .w(Len::Fill)
                        .h_px(t::SP_8)
                        .align(Align::End)
                        .child(|r| {
                            r.text((), h, 16.0).color(t::TEXT);
                        });
                }
                for row in &group.rows {
                    show_all_row(c, icons, row, &on_navigate, &on_play, &on_context_menu);
                }
            }
        });
}

/// One full-width row: thumb + title/subtitle (+ trailing chevron for
/// rows that open a detail page; play rows have no nav affordance).
fn show_all_row(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    row: &ShowAllRow,
    nav: &NavFn,
    play: &PlayFn,
    on_context_menu: &CtxMenuFn,
) {
    let radius = if row.round { t::R_FULL } else { t::R_SM };
    let chevron = matches!(row.action, RowAction::Open(_));
    let mut r = s.row(());
    r.w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE);
    match &row.action {
        RowAction::Open(target) => {
            let nav = nav.clone();
            let target = target.clone();
            r.on_click(move |ctx| nav(ctx, target.clone()));
        }
        RowAction::Play(target) => {
            let play = play.clone();
            let target = target.clone();
            r.on_click(move |_| play(target.clone()));
        }
    }
    // Right-click → context menu (track rows only; same gesture as every
    // other track list).
    if let Some(target) = row.menu.clone() {
        crate::views::home::attach_context_menu(&mut r, on_context_menu, target);
    }
    r.child(|r| {
        // Thumb.
        r.col(()).w_px(t::THUMB_MD).h_px(t::THUMB_MD).child(|b| {
            if let Some(sig) = row.thumb.clone() {
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
        // Title + subtitle.
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), &row.title, 14.0)
                    .color(t::TEXT)
                    .max_width_px(420.0);
                m.text((), &row.subtitle, 12.0)
                    .color(t::TEXT_DIM)
                    .max_width_px(420.0);
            });
        // Trailing chevron affordance (detail-page rows only).
        if chevron {
            r.row(()).push_end().w_px(t::SP_6).center().child(|c| {
                icons.render(c, Icon::ChevronRight, t::ICON_MD, t::TEXT_DIM);
            });
        }
    });
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Today's + yesterday's date as `YYYY-MM-DD` (UTC — matches Spotify's
/// `played_at` which is UTC; good enough for day bucketing).
pub fn today_yesterday() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    (date_string(days), date_string(days - 1))
}

/// Day-group label for a `played_at` timestamp: "Today" / "Yesterday" /
/// "June 5" / a bare date fallback.
pub fn day_label(played_at: &str, today: &str, yesterday: &str) -> String {
    let date = played_at.get(..10).unwrap_or("");
    if date == today {
        "Today".to_string()
    } else if date == yesterday {
        "Yesterday".to_string()
    } else if let Some((y, m, d)) = parse_ymd(date) {
        let _ = y;
        format!("{} {}", MONTHS[(m as usize).clamp(1, 12) - 1], d)
    } else {
        "Earlier".to_string()
    }
}

fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.splitn(3, '-');
    let y = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    Some((y, m, d))
}

fn date_string(days_since_epoch: i64) -> String {
    let (y, m, d) = civil_from_days(days_since_epoch);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-epoch → (year, month, day). Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}
