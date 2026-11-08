//! Toasts + the notification history panel — the app-wide notice layer.
//!
//! Built as its own scene root *after* whichever view is mounted (see
//! `main`), so toasts float above every view and every modal, on Setup and
//! Login as much as Home. The root itself is transparent and handler-free:
//! it neither paints nor takes input; only the cards do.
//!
//! Each card is a fixed-size, layer-promoted node at the stack origin whose
//! slot, slide and fade are composite binds on [`ToastModel`] signals — the
//! stack animates without relayout, and (since the engine keeps hit rects
//! on composite offsets) hover and clicks land where the card is drawn.

use std::rc::Rc;

use opal_gfx::{Align, Computed, CursorIcon, Len, Scene};

use crate::model::toasts::{HistoryEntry, LiveToast, ToastModel};
use crate::notify::Severity;
use crate::widgets::button::{icon_button, text_action};
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Card size (logical px). Fixed so the stack's slots are known without a
/// measure: title and body are single ellipsized lines — the history panel
/// carries the full text.
pub const TOAST_W: f32 = 360.0;
pub const TOAST_H: f32 = 64.0;
/// Gap between stacked cards.
pub const GAP: f32 = t::SP_2;
/// Inset from the window's right edge.
pub const EDGE: f32 = t::SP_3;
/// Top of the stack: clear of the top bar (its button + vertical pad) with
/// one gap below it.
pub const STACK_TOP: f32 = t::TOPBAR_BTN + 2.0 * t::SP_2 + GAP;

const PAD: f32 = t::SP_3;
const STRIPE_W: f32 = t::SP_1;
const CLOSE_BTN: f32 = t::SP_6;
/// Width left for the card's text lines.
const TEXT_W: f32 = TOAST_W - 2.0 * PAD - STRIPE_W - CLOSE_BTN - 2.0 * t::SP_3;

const PANEL_W: f32 = 420.0;
/// The history list's scroll viewport height.
const LIST_H: f32 = 420.0;
const PANEL_PAD: f32 = t::SP_4;
const PANEL_GAP: f32 = t::SP_3;
/// Header row height — fixed (the "Clear all" pill's height) so the morph
/// targets below are exact whether or not the pill is there.
const HEADER_H: f32 = t::BTN_H_SM;
/// The empty-state row.
const EMPTY_H: f32 = t::SP_16;

/// Height the history panel morphs *out from* on open — pad + header only,
/// like the other popups.
pub fn panel_collapsed_h() -> f32 {
    2.0 * PANEL_PAD + HEADER_H
}

/// Height the history panel morphs *to*: the header plus either the
/// empty-state row or the scrolling list.
pub fn panel_target_h(empty: bool) -> f32 {
    panel_collapsed_h() + PANEL_GAP + if empty { EMPTY_H } else { LIST_H }
}
/// History bodies are clipped to this many chars on screen (the clipboard
/// copy is never clipped).
const BODY_MAX: usize = 480;
const DOT: f32 = t::SP_1_5;

/// The layer's emitters, built once — clone-cheap.
#[derive(Clone)]
pub struct ToastActions {
    /// Close a toast early.
    pub dismiss: Rc<dyn Fn(u64)>,
    /// Open the notification history.
    pub open_history: Rc<dyn Fn()>,
    /// Empty the notification history.
    pub clear_history: Rc<dyn Fn()>,
}

/// The notice layer for one build.
pub struct Toasts<'a> {
    pub model: &'a ToastModel,
    pub icons: &'a Rc<IconSet>,
    pub actions: &'a ToastActions,
}

impl Component for Toasts<'_> {
    fn view(&self, s: &mut Scene) {
        s.col("toast_root").fill().child(|root| {
            root.row(())
                .w(Len::Fill)
                .pad_ltrb(t::SP_0, STACK_TOP, EDGE, t::SP_0)
                .justify(opal_gfx::Justify::End)
                .child(|r| {
                    // Zero-height column: the cards sit absolutely at its
                    // origin and take their slots from composite offsets.
                    // Oldest first so newer cards paint (and hit) on top
                    // while they overlap mid-reflow.
                    r.col(()).w_px(TOAST_W).child(|stack| {
                        for toast in self.model.live.iter().rev() {
                            card(stack, toast, self.icons, self.actions);
                        }
                    });
                });
            history_panel(root, self.model, self.actions);
        });
    }
}

/// Foreground colour for a severity.
pub fn severity_color(s: Severity) -> [f32; 4] {
    match s {
        Severity::Warning => t::WARNING,
        Severity::Error => t::DANGER,
    }
}

fn card(s: &mut Scene, toast: &LiveToast, icons: &IconSet, actions: &ToastActions) {
    let color = severity_color(toast.severity);
    let open = actions.open_history.clone();
    let dismiss = actions.dismiss.clone();
    let id = toast.id;
    let title = match toast.count {
        1 => toast.title.clone(),
        n => format!("{} \u{00d7}{n}", toast.title),
    };
    s.row(())
        .abs(0.0, 0.0)
        .w_px(TOAST_W)
        .h_px(TOAST_H)
        .pad(PAD)
        .gap(t::SP_3)
        .align(Align::Center)
        .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
        .radius(t::R_LG)
        .border(1.0, t::with_alpha(color, 0.28))
        .layer_offset_x(toast.slide.clone())
        .layer_offset_y(toast.slot.clone())
        .layer_opacity(toast.opacity.clone())
        .on_hover(toast.hover.clone())
        .cursor(CursorIcon::Pointer)
        .on_click(move |_| open())
        .child(|c| {
            c.rect(())
                .w_px(STRIPE_W)
                .h(Len::Fill)
                .radius(t::R_FULL)
                .color(color);
            c.col(()).w(Len::Fill).gap(t::SP_1).child(|txt| {
                txt.text((), &title, t::TEXT_SM)
                    .color(t::TEXT)
                    .max_width_px(TEXT_W);
                txt.text((), &toast.body, t::TEXT_XS)
                    .color(t::TEXT_DIM)
                    .max_width_px(TEXT_W);
            });
            icon_button(c, icons, Icon::Close, CLOSE_BTN, t::ICON_XS, move |_| {
                dismiss(id)
            });
        });
}

fn history_panel(s: &mut Scene, model: &ToastModel, actions: &ToastActions) {
    // Nothing to build while it's closed: the list can run to hundreds of
    // wrapped entries.
    let shown = model.overlay.is_shown();
    // Same shell as the devices / like popups: dimming scrim, solid panel,
    // and a height that springs collapsed ↔ content on open / close.
    model.overlay.render(s, t::SCRIM, |host| {
        host.col(())
            .w_px(PANEL_W)
            .height_px_bind(model.overlay.morph_height())
            .clip()
            .pad(PANEL_PAD)
            .gap(PANEL_GAP)
            .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
            .radius(t::R_LG)
            .border(1.0, t::BORDER)
            .child(|p| {
                if !shown {
                    return;
                }
                p.row(())
                    .w(Len::Fill)
                    .h_px(HEADER_H)
                    .align(Align::Center)
                    .child(|h| {
                        h.text((), "Notifications", t::TEXT_LG).color(t::TEXT);
                        if !model.history.is_empty() {
                            let clear = actions.clear_history.clone();
                            h.row(()).push_end().child(|a| {
                                text_action(a, "Clear all", move |_| clear());
                            });
                        }
                    });
                if model.history.is_empty() {
                    p.row(()).w(Len::Fill).h_px(EMPTY_H).center().child(|e| {
                        e.text((), "No warnings or errors", t::TEXT_SM)
                            .color(t::TEXT_DIM);
                    });
                    return;
                }
                p.col(())
                    .w(Len::Fill)
                    .h_px(LIST_H)
                    .gap(t::SP_1)
                    .scroll_y()
                    .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
                    .child(|list| {
                        for e in &model.history {
                            history_row(list, e);
                        }
                    });
            });
    });
}

fn history_row(s: &mut Scene, e: &HistoryEntry) {
    let color = severity_color(e.severity);
    let copy = e.to_text();
    let heading = e.title.as_deref().unwrap_or(match e.severity {
        Severity::Warning => "Warning",
        Severity::Error => "Error",
    });
    let when = match e.count {
        1 => e.at.format("%H:%M:%S").to_string(),
        n => format!("{} \u{00d7}{n}", e.at.format("%H:%M:%S")),
    };
    let body: String = match e.body.char_indices().nth(BODY_MAX) {
        Some((cut, _)) => format!("{}\u{2026}", &e.body[..cut]),
        None => e.body.clone(),
    };
    s.col(())
        .w(Len::Fill)
        .pad(t::SP_2_5)
        .gap(t::SP_1)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .cursor(CursorIcon::Pointer)
        .hover_hint("Click to copy")
        .on_click(move |ctx| ctx.tree.request_clipboard(copy.clone()))
        .child(|r| {
            r.row(())
                .w(Len::Fill)
                .gap(t::SP_2)
                .align(Align::Center)
                .child(|l| {
                    l.rect(())
                        .w_px(DOT)
                        .h_px(DOT)
                        .radius(t::R_FULL)
                        .color(color);
                    l.text((), heading, t::TEXT_SM)
                        .color(t::TEXT)
                        .max_width_px(PANEL_W - t::SP_32);
                    l.text((), &when, t::TEXT_XS).color(t::TEXT_DIM).push_end();
                });
            r.text((), &body, t::TEXT_XS)
                .color(t::TEXT_DIM)
                .w(Len::Fill)
                .wrap();
            r.text((), &e.target, t::TEXT_XS)
                .color(t::with_alpha(t::TEXT_DIM, 0.6));
        });
}

/// The bell's badge colour: the worst unseen severity, or nothing.
pub fn badge_color(model: &ToastModel) -> Computed<[f32; 4]> {
    Computed::new((model.unseen.clone(),), |(u,)| {
        u.map_or([0.0; 4], severity_color)
    })
}
