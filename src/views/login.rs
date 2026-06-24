//! The **Login** view — the post-setup, pre-Home screen.
//!
//! Reached once a client id is configured (from [`super::setup`] after a
//! save, or directly on launch when one is already stored). While mounted
//! as `View::Splash` it shows "Checking saved credentials…" during the
//! startup token-load; as `View::Login` it shows the "Log in with Spotify"
//! button.
//!
//! Two escape hatches sit in the corners:
//! - top-left **Back** → return to the setup view to edit the client id
//!   (non-destructive).
//! - bottom-left **Reset preferences** → wipe all prefs + stored tokens and
//!   bounce back to setup.

use std::cell::RefCell;
use std::rc::Rc;

use opal_gfx::{Align, Computed, EventCtx, Len, Scene};

use crate::app::AppState;
use crate::app::msg::{Dispatch, Msg};
use crate::views::View;
use crate::widgets::button::{ButtonTone, pill_button};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::{chrome, tokens};

/// The Login view controller — owns the OAuth-start, back, and reset
/// callbacks (pure `Msg` emitters; the logic lives in `app::update`).
pub struct LoginView {
    state: Rc<RefCell<AppState>>,
    icons: Rc<IconSet>,
    dispatch: Dispatch,
}

impl LoginView {
    pub fn new(state: Rc<RefCell<AppState>>, dispatch: Dispatch, icons: Rc<IconSet>) -> Self {
        Self { state, icons, dispatch }
    }

    pub fn build(&self, s: &mut Scene) {
        // Splash = the startup token-load is still running → show "checking".
        let checking = matches!(self.state.borrow().router.view, View::Splash);
        // Back is only meaningful when we arrived here from Setup (the user
        // just entered a client id) — not on a fresh launch or after logout.
        let show_back = !checking && self.state.borrow().router.came_from_setup;
        let state = self.state.clone();

        // Corner actions — each captures its own handle clone into a
        // 'static event closure.
        let on_back = {
            let me = self.handle();
            move |_: &mut EventCtx| me.back_to_setup()
        };
        let on_reset = {
            let me = self.handle();
            move |_: &mut EventCtx| me.reset_prefs()
        };
        let on_login = {
            let me = self.handle();
            move |_: &mut EventCtx| me.start_login()
        };

        s.col(())
            .fill()
            .rgba(tokens::BG[0], tokens::BG[1], tokens::BG[2], 1.0)
            .child(|root| {
                chrome::title_bar(root, &self.icons, "Opal");

                root.col(())
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .child(|body| {
                        // Top-left: back to setup — only when we came from it.
                        if show_back {
                            body.row(()).w(Len::Fill).h(Len::Auto).pad(16.0).child(|tr| {
                                pill_button(
                                    tr,
                                    &self.icons,
                                    "Back",
                                    Some(Icon::ChevronLeft),
                                    ButtonTone::Neutral,
                                    on_back,
                                );
                            });
                        }

                        // Centre fills the gap between the corners → title +
                        // button sit dead-centre. Entrance fade only (a slide
                        // here would need an abs wrapper, which would drop the
                        // corners out of flow). `h(Fill)` pushes Reset to the
                        // bottom.
                        let fade = Computed::new(
                            (state.borrow().router.view_t.clone(),),
                            |(tt,)| tt.clamp(0.0, 1.0),
                        );
                        body.col(())
                            .w(Len::Fill)
                            .h(Len::Fill)
                            .center()
                            .gap(20.0)
                            .opacity_bind(fade)
                            .child(|c| {
                                logo_title(c, &self.icons);
                                c.text((), "An unofficial Spotify desktop client.", tokens::TEXT_BASE)
                                    .color(tokens::TEXT_DIM);
                                if checking {
                                    c.text((), "Checking saved credentials...", tokens::TEXT_SM)
                                        .color(tokens::TEXT_DIM);
                                } else {
                                    login_button(c, on_login);
                                }
                            });

                        // Bottom-left: destructive reset (hidden mid-check).
                        if !checking {
                            body.row(()).w(Len::Fill).h(Len::Auto).pad(16.0).child(|br| {
                                pill_button(
                                    br,
                                    &self.icons,
                                    "Reset preferences",
                                    None,
                                    ButtonTone::Danger,
                                    on_reset,
                                );
                            });
                        }
                    });
            });
    }

    /// Capture-friendly handle for 'static event closures — holds only the
    /// `Dispatch` (no `AppState`), so the callbacks just emit intents.
    fn handle(&self) -> LoginHandle {
        LoginHandle {
            dispatch: self.dispatch.clone(),
        }
    }
}

/// The slice of [`LoginView`] the event closures need.
#[derive(Clone)]
struct LoginHandle {
    dispatch: Dispatch,
}

impl LoginHandle {
    fn start_login(&self) {
        self.dispatch.send(Msg::StartLogin);
    }
    fn back_to_setup(&self) {
        self.dispatch.send(Msg::BackToSetup);
    }
    fn reset_prefs(&self) {
        self.dispatch.send(Msg::ResetPrefs);
    }
}

/// The brand header — the logo mark beside the "Opal" wordmark, centered.
/// (Static for now; a fluttering-wings animation is planned.)
fn logo_title(c: &mut Scene, icons: &IconSet) {
    c.row(())
        .align(Align::Center)
        .gap(tokens::SP_3)
        .child(|r| {
            icons.render_logo(r, 56.0);
            r.text((), "Opal", tokens::TEXT_4XL).color(tokens::TEXT);
        });
}

/// The "Log in with Spotify" pill.
fn login_button(c: &mut Scene, on_login: impl Fn(&mut EventCtx) + 'static) {
    c.row(())
        .w_px(240.0)
        .h_px(48.0)
        .color(tokens::ACCENT)
        .hover_color(tokens::ACCENT_HOVER)
        .radius(24.0)
        .center()
        .on_click(on_login)
        .child(|b| {
            b.text((), "Log in with Spotify", tokens::TEXT_BASE)
                .color([1.0, 1.0, 1.0, 1.0]);
        });
}
