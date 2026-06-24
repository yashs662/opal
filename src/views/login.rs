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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opal_gfx::{Align, Computed, EventCtx, Len, Scene};

use crate::app::AppState;
use crate::views::View;
use crate::widgets::button::{ButtonTone, pill_button};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::{chrome, tokens};
use crate::worker::Worker;

/// The Login view controller — owns the OAuth-start, back, and reset
/// callbacks.
pub struct LoginView {
    state: Rc<RefCell<AppState>>,
    worker: Rc<Worker>,
    icons: Rc<IconSet>,
    rebuild: Rc<Cell<bool>>,
}

impl LoginView {
    pub fn new(
        state: Rc<RefCell<AppState>>,
        worker: Rc<Worker>,
        icons: Rc<IconSet>,
        rebuild: Rc<Cell<bool>>,
    ) -> Self {
        Self { state, worker, icons, rebuild }
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
            move |ctx: &mut EventCtx| me.back_to_setup(ctx.timeline, ctx.now)
        };
        let on_reset = {
            let me = self.handle();
            move |ctx: &mut EventCtx| me.reset_prefs(ctx.timeline, ctx.now)
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

    /// Capture-friendly `Rc` handle for 'static event closures.
    fn handle(&self) -> LoginHandle {
        LoginHandle {
            state: self.state.clone(),
            worker: self.worker.clone(),
            rebuild: self.rebuild.clone(),
        }
    }
}

/// The slice of [`LoginView`] the event closures need.
#[derive(Clone)]
struct LoginHandle {
    state: Rc<RefCell<AppState>>,
    worker: Rc<Worker>,
    rebuild: Rc<Cell<bool>>,
}

impl LoginHandle {
    fn start_login(&self) {
        if let Some(id) = self.state.borrow().prefs.data.client_id() {
            self.worker.start_oauth(id);
        }
    }
    fn back_to_setup(&self, tl: &mut opal_gfx::Timeline, now: std::time::Instant) {
        self.state.borrow_mut().router.go_view(View::Setup, tl, now);
        self.rebuild.set(true);
    }
    fn reset_prefs(&self, tl: &mut opal_gfx::Timeline, now: std::time::Instant) {
        let mut st = self.state.borrow_mut();
        st.prefs.reset();
        st.auth.sign_out();
        st.router.go_view(View::Setup, tl, now);
        self.rebuild.set(true);
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
