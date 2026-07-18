//! Connect-devices popup — pick where playback happens.
//!
//! An [`Overlay`]-hosted panel (same primitive as settings: scrim, fade,
//! click-out dismiss) listing every Connect device on the account. The
//! active device is accent-highlighted; clicking any other row transfers
//! playback there (and resumes). The list is fetched fresh on every open.

use std::rc::Rc;

use opal_gfx::{Align, Len, Scene, Signal};

use crate::model::DevicesModel;
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

pub struct DevicesPanel<'a> {
    pub devices: &'a DevicesModel,
    pub accent: &'a Signal<[f32; 4]>,
    pub icons: &'a Rc<IconSet>,
    /// Transfer playback to the device id (wired to the worker).
    pub on_transfer: Rc<dyn Fn(String)>,
}

/// One device row's height + the panel padding — the geometry the morph
/// target is summed from. The list has no scroller, so the target must fit
/// every row exactly (slight over is fine; under would clip a device).
const ROW_H: f32 = t::SP_14;

/// Height the panel morphs *out from* on open — pad + the title line only.
pub fn collapsed_h() -> f32 {
    t::SP_5 * 2.0 + t::SP_6
}

/// Height the panel morphs *to* for `n` device rows (or the empty-state row).
pub fn target_h(n: usize) -> f32 {
    let base = collapsed_h();
    if n == 0 {
        base + t::SP_3 + t::SP_12
    } else {
        base + n as f32 * (t::SP_3 + ROW_H)
    }
}

impl Component for DevicesPanel<'_> {
    fn view(&self, s: &mut Scene) {
        let icons = self.icons;
        let list = self.devices.list.clone();
        let active_id = self.devices.active_id.clone();
        let self_id = self.devices.self_id.clone();
        let overlay = self.devices.overlay.clone();
        let accent = self.accent.clone();
        let on_transfer = self.on_transfer.clone();
        self.devices.overlay.render(s, t::SCRIM, move |host| {
            host.col(())
                .w_px(t::SP_80)
                // Morphs collapsed → the row count on open (and back on
                // close/dismiss), matching the settings/search modals.
                .height_px_bind(overlay.morph_height())
                .clip()
                .pad(t::SP_5)
                .gap(t::SP_3)
                .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
                .radius(t::R_LG)
                .border(1.0, t::BORDER)
                .child(move |panel| {
                    panel.text((), "Connect to a device", 18.0).color(t::TEXT);
                    if list.is_empty() {
                        panel
                            .row(())
                            .w(Len::Fill)
                            .h_px(t::SP_12)
                            .center()
                            .child(|e| {
                                e.text((), "No devices found", 13.0).color(t::TEXT_DIM);
                            });
                    }
                    for d in &list {
                        // REST `is_active` can lag the cluster push — the
                        // cluster's active id wins when present.
                        let active = if active_id.is_empty() {
                            d.is_active
                        } else {
                            d.id == active_id
                        };
                        let is_self = d.id == self_id;
                        device_row(
                            panel,
                            icons,
                            d,
                            active,
                            is_self,
                            &accent,
                            &overlay,
                            &on_transfer,
                        );
                    }
                });
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn device_row(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    d: &crate::api::Device,
    active: bool,
    is_self: bool,
    accent: &Signal<[f32; 4]>,
    overlay: &opal_gfx::Overlay,
    on_transfer: &Rc<dyn Fn(String)>,
) {
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(t::SP_14)
        .pad_xy(t::SP_3, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD);
    if active {
        // The playing device: highlighted, not clickable (transferring
        // to the active device is a no-op).
        row.rgba(1.0, 1.0, 1.0, 0.06);
    } else {
        let id = d.id.clone();
        let overlay = overlay.clone();
        let on_transfer = on_transfer.clone();
        row.hover_color(t::HOVER_LIFT_SUBTLE).on_click(move |ctx| {
            on_transfer(id.clone());
            overlay.morph_close(ctx.timeline, ctx.now);
        });
    }
    let accent = accent.clone();
    let name = d.name.clone();
    let kind = d.kind.clone();
    row.child(move |r| {
        // Type glyph — accent-tinted on the active device.
        r.row(()).w_px(t::SP_8).center().child(|c| {
            if active {
                icons.render(c, Icon::Devices, t::ICON_MD, accent.clone());
            } else {
                icons.render(c, Icon::Devices, t::ICON_MD, t::TEXT_DIM);
            }
        });
        r.col(()).w(Len::Fill).gap(t::SP_0_5).child(|m| {
            m.text((), &name, 14.0).color(t::TEXT).max_width_px(260.0);
            let sub = match (active, is_self) {
                (true, _) => "Playing".to_string(),
                (false, true) => format!("{kind} \u{2022} This device"),
                (false, false) => kind.clone(),
            };
            m.text((), &sub, 12.0).color(t::TEXT_DIM);
        });
    });
}
