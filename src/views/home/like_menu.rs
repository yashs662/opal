//! Playlist picker — the popup behind the player-bar like icon.
//!
//! An [`Overlay`]-hosted panel (same primitive as the devices/settings
//! popups: scrim, fade, click-out dismiss). It lists "Liked Songs" plus
//! every editable playlist with a checkbox showing whether the current
//! track is in it; toggling a checkbox optimistically adds/removes the
//! track (the worker confirms, rolling back on failure). The membership
//! index lives in the worker — this view only reads the lightweight
//! playlist list + the current track's membership set.

use std::rc::Rc;

use opal_gfx::{Align, Len, Scene, Signal};

use crate::model::MembershipModel;
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Fixed height of the scrolling playlist list — tall enough to show ~7
/// rows before scrolling, matching the picker's popup feel.
const LIST_H: f32 = 300.0;

pub struct LikeMenu<'a> {
    pub membership: &'a MembershipModel,
    pub accent: &'a Signal<[f32; 4]>,
    pub icons: &'a Rc<IconSet>,
    /// Add/remove the target track from playlist `id` (`add` = check on).
    pub on_toggle_playlist: Rc<dyn Fn(String, bool)>,
    /// Save/unsave the target track to Liked Songs (`add` = check on).
    pub on_toggle_liked: Rc<dyn Fn(bool)>,
}

impl Component for LikeMenu<'_> {
    fn view(&self, s: &mut Scene) {
        let icons = self.icons;
        let accent = self.accent.clone();
        let playlists = self.membership.playlists.clone();
        // Rows need both the playlist list AND the target's membership
        // answer (async for row-heart targets — instant once the worker
        // index is warm).
        let ready = self.membership.ready && self.membership.target_ready;
        let target = self.membership.target.clone();
        let liked_now = self.membership.target_liked;
        // Pre-read membership for each playlist row (static read + rebuild on
        // toggle keeps the checkbox honest without a per-row signal).
        let in_set: Vec<bool> = playlists
            .iter()
            .map(|p| self.membership.target_contains(&p.id))
            .collect();
        let on_toggle_playlist = self.on_toggle_playlist.clone();
        let on_toggle_liked = self.on_toggle_liked.clone();
        self.membership.overlay.render(s, t::SCRIM, move |host| {
            host.col(())
                .w_px(t::SP_80)
                .pad(t::SP_5)
                .gap(t::SP_3)
                .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
                .radius(t::R_LG)
                .border(1.0, t::BORDER)
                .child(move |panel| {
                    // Header: "Add to playlist" + the track it acts on.
                    panel.text((), "Add to playlist", 18.0).color(t::TEXT);
                    if !target.track.name.is_empty() {
                        panel
                            .text(
                                (),
                                format!("{} \u{2022} {}", target.track.name, target.track.artist),
                                12.0,
                            )
                            .color(t::TEXT_DIM)
                            .max_width_px(t::SP_80 - t::SP_10);
                    }
                    if !ready {
                        panel
                            .row(())
                            .w(Len::Fill)
                            .h_px(t::SP_12)
                            .center()
                            .child(|e| {
                                e.text((), "Loading playlists\u{2026}", 13.0)
                                    .color(t::TEXT_DIM);
                            });
                        return;
                    }
                    // One scroll list: Liked Songs first (so it scrolls with
                    // the rest), then every editable playlist. The right inset
                    // is the scrollbar gutter.
                    panel
                        .col(())
                        .w(Len::Fill)
                        .h_px(LIST_H)
                        .gap(t::SP_0_5)
                        .pad_ltrb(t::SP_0, t::SP_0, t::SP_2, t::SP_0)
                        .scroll_y()
                        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
                        .child(move |list| {
                            let liked_next = !liked_now;
                            check_row(list, icons, "Liked Songs", liked_now, &accent, move || {
                                on_toggle_liked(liked_next)
                            });
                            for (p, on) in playlists.iter().zip(in_set) {
                                let id = p.id.clone();
                                let on_toggle = on_toggle_playlist.clone();
                                let next = !on;
                                check_row(list, icons, &p.name, on, &accent, move || {
                                    on_toggle(id.clone(), next)
                                });
                            }
                        });
                });
        });
    }
}

/// One toggleable row: a playlist name + a checkbox (accent-filled with a
/// check glyph when on, an empty bordered box when off). The whole row is
/// clickable.
fn check_row(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    name: &str,
    on: bool,
    accent: &Signal<[f32; 4]>,
    toggle: impl Fn() + 'static,
) {
    let accent = accent.clone();
    let name = name.to_string();
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_11)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .on_click(move |_| toggle())
        .child(move |r| {
            r.text((), &name, 14.0)
                .color(t::TEXT)
                .max_width_px(t::SP_56);
            // Checkbox pushed to the trailing edge.
            let mut box_node = r.row(());
            box_node
                .push_end()
                .w_px(t::SP_5)
                .h_px(t::SP_5)
                .center()
                .radius(t::R_SM);
            if on {
                box_node.color(accent.clone());
                box_node.child(|b| {
                    icons.render(b, Icon::Check, t::ICON_SM, t::PANEL);
                });
            } else {
                box_node.border(1.5, t::TEXT_DIM);
            }
        });
}
