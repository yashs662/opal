//! Top chrome bar — window drag region, nav arrows, search, settings/bell,
//! and the min/max/close window buttons. A [`Component`].

use std::rc::Rc;

use opal_gfx::{Align, Computed, Len, Overlay, Scene, Signal, WindowAction};

use crate::widgets::chrome::chrome_btn;
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

pub struct TopBar<'a> {
    /// The settings modal — opened by the gear button (the bar owns the
    /// open gesture; the `Overlay` itself owns the scrim/fade).
    pub settings: &'a Overlay,
    /// Measure cache usage + rebuild when the modal opens.
    pub on_settings_open: Rc<dyn Fn()>,
    /// History arrows: whether each direction has anywhere to go (the
    /// glyphs dim to inert when not) + the step emitters.
    pub can_back: &'a Signal<bool>,
    pub can_forward: &'a Signal<bool>,
    pub on_back: Rc<dyn Fn()>,
    pub on_forward: Rc<dyn Fn()>,
    pub icons: &'a Rc<IconSet>,
}

impl Component for TopBar<'_> {
    fn view(&self, s: &mut Scene) {
        let icons = self.icons;
        s.row("topbar")
            .w(Len::Fill)
            .h(Len::Auto)
            .pad_ltrb(t::SP_2, t::SP_2, t::SP_2, t::SP_2)
            .gap(t::SP_2)
            .align(Align::Center)
            .rgba(0.0, 0.0, 0.0, 0.0)
            .window_action(WindowAction::DragMove)
            .child(|t_row| {
                topbar_icon_btn(t_row, icons, Icon::Menu);
                nav_arrow(
                    t_row,
                    icons,
                    Icon::ChevronLeft,
                    self.can_back,
                    self.on_back.clone(),
                );
                nav_arrow(
                    t_row,
                    icons,
                    Icon::ChevronRight,
                    self.can_forward,
                    self.on_forward.clone(),
                );

                t_row
                    .row(())
                    .w(Len::Fill)
                    .h_px(t::SEARCH_H)
                    .center()
                    .child(|c| {
                        c.row(())
                            .w_px(t::SEARCH_W)
                            .h_px(t::SEARCH_H)
                            .pad_xy(t::SP_3_5, t::SP_0)
                            .gap(t::SP_2_5)
                            .align(Align::Center)
                            .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
                            .radius(t::R_FULL)
                            .border(1.0, t::BORDER)
                            .child(|s2| {
                                icons.render(s2, Icon::Search, t::ICON_SM, t::TEXT_DIM);
                                s2.text((), "What do you want to play?", 13.0).color(t::TEXT_DIM);
                            });
                    });

                let settings = self.settings.clone();
                let on_settings_open = self.on_settings_open.clone();
                topbar_icon_btn_click(t_row, icons, Icon::Settings, move |ctx| {
                    settings.open(ctx.timeline, ctx.now);
                    on_settings_open();
                });
                topbar_icon_btn(t_row, icons, Icon::Bell);

                chrome_btn(t_row, icons, Icon::Minimize, WindowAction::Minimize, t::BTN_HOVER, true);
                chrome_btn(
                    t_row,
                    icons,
                    Icon::Maximize,
                    WindowAction::ToggleMaximize,
                    t::BTN_HOVER,
                    false,
                );
                chrome_btn(t_row, icons, Icon::Close, WindowAction::Close, t::CLOSE_HOVER, false);
                // ^ window controls share the canonical widget in `chrome`.
            });
    }
}

/// Top-bar pill button with a click handler (e.g. the settings gear). The
/// handler receives the full `EventCtx` so it can start a timeline tween
/// (the settings fade) at click time.
fn topbar_icon_btn_click(
    s: &mut Scene,
    icons: &IconSet,
    icon: Icon,
    on_click: impl Fn(&mut opal_gfx::EventCtx) + 'static,
) {
    s.row(())
        .w_px(t::TOPBAR_BTN)
        .h_px(t::TOPBAR_BTN)
        .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
        .hover_color(t::PANEL_HI)
        .radius(t::R_FULL)
        .center()
        .on_click(on_click)
        .child(|c| {
            icons.render(c, icon, t::ICON_MD, t::TEXT);
        });
}

/// A history arrow — the glyph dims to inert when its direction is
/// empty; clicks always emit (the handler no-ops on empty history).
fn nav_arrow(s: &mut Scene, icons: &IconSet, icon: Icon, can: &Signal<bool>, go: Rc<dyn Fn()>) {
    let tint = Computed::new((can.clone(),), |(c,)| {
        if c { t::TEXT } else { [1.0, 1.0, 1.0, 0.25] }
    });
    s.row(())
        .w_px(t::TOPBAR_BTN)
        .h_px(t::TOPBAR_BTN)
        .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
        .hover_color(t::PANEL_HI)
        .radius(t::R_FULL)
        .center()
        .on_click(move |_| go())
        .child(|c| {
            icons.render(c, icon, t::ICON_MD, tint.clone());
        });
}

fn topbar_icon_btn(s: &mut Scene, icons: &IconSet, icon: Icon) {
    s.row(())
        .w_px(t::TOPBAR_BTN)
        .h_px(t::TOPBAR_BTN)
        .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 1.0)
        .hover_color(t::PANEL_HI)
        .radius(t::R_FULL)
        .center()
        .child(|c| {
            icons.render(c, icon, t::ICON_MD, t::TEXT);
        });
}

