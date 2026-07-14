use opal_gfx::{Align, Len, Scene, WindowAction};

use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens;

pub fn title_bar(s: &mut Scene, icons: &IconSet, title: &str) {
    s.row(())
        .w(Len::Fill)
        .h_px(36.0)
        .pad_xy(10.0, 0.0)
        .gap(8.0)
        .align(Align::Center)
        .rgba(tokens::PANEL[0], tokens::PANEL[1], tokens::PANEL[2], 1.0)
        .window_action(WindowAction::DragMove)
        .child(|t| {
            t.text((), title, 13.0).color(tokens::TEXT_DIM);

            chrome_btn(
                t,
                icons,
                Icon::Minimize,
                WindowAction::Minimize,
                tokens::BTN_HOVER,
                true,
            );
            chrome_btn(
                t,
                icons,
                Icon::Maximize,
                WindowAction::ToggleMaximize,
                tokens::BTN_HOVER,
                false,
            );
            chrome_btn(
                t,
                icons,
                Icon::Close,
                WindowAction::Close,
                tokens::CLOSE_HOVER,
                false,
            );
        });
}

/// A window-control button (minimize / maximize / close). The single
/// source of truth for window-control chrome — reused by both the login
/// title bar and the Home top bar so they stay visually identical.
/// `push_end` shoves the button (and the trailing siblings that follow it)
/// to the right edge.
pub fn chrome_btn(
    s: &mut Scene,
    icons: &IconSet,
    icon: Icon,
    action: WindowAction,
    hover: [f32; 4],
    push_end: bool,
) {
    let mut b = s.row(());
    b.w_px(tokens::SP_11)
        .h_px(tokens::SP_8)
        .rgba(0.0, 0.0, 0.0, 0.0)
        .hover_color(hover)
        .radius(tokens::R_MD)
        .center()
        .window_action(action);
    if push_end {
        b.push_end();
    }
    b.child(|c| {
        icons.render(c, icon, tokens::ICON_XS, tokens::TEXT);
    });
}
