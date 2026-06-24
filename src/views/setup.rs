//! The first-run **Setup** view — a standalone top-level view (not part of
//! Login) that captures the user's own Spotify client id.
//!
//! Opal ships no bundled client id: the OAuth consent screen names the
//! registering app, so every user creates their own Spotify app and pastes
//! its Client ID here. Shown when no id is configured (fresh install, or
//! after a preferences reset on the login screen); saving it eases over to
//! the Login view.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use opal_gfx::{
    Align, Computed, Curve, EventCtx, Justify, Len, NodeId, Scene, Signal, TextSignal,
};

use crate::app::AppState;
use crate::constants::SPOTIFY_REDIRECT_URI;
use crate::views::View;
use crate::widgets::icon::IconSet;
use crate::widgets::{chrome, tokens};

/// Dashboard setup steps, shown in order on the setup card.
const INSTRUCTIONS: &[&str] = &[
    "1.  Open developer.spotify.com/dashboard and log in.",
    "2.  Click \"Create app\". Name and description can be anything.",
    "3.  Add the redirect URI below, and tick the \"Web API\" scope.",
    "4.  Save, then open the app's Settings.",
    "5.  Copy the \"Client ID\" and paste it here.",
];

/// The Setup view controller — owns the client-id-save callback and the
/// live paste-field draft.
pub struct SetupView {
    state: Rc<RefCell<AppState>>,
    icons: Rc<IconSet>,
    rebuild: Rc<Cell<bool>>,
    /// Live mirror of the paste-field value, so the Save button can read it
    /// without the field re-emitting on click. The field owns its own
    /// editor state across rebuilds; this only feeds the button.
    draft: Rc<RefCell<String>>,
    /// Copy-feedback phase for the redirect-URI pill: set to 1.0 on click,
    /// then tweened back to 0.0 over a couple seconds. Drives the green
    /// highlight (thresholded → instant on/off, no fade) reactively (no
    /// rebuild). A `Signal` is cheap to clone (refcounted).
    copied: Signal<f32>,
    /// Reactive text for the pill's trailing hint — flips between "Click to
    /// copy" and "Copied!" instantly, derived from `copied`'s threshold.
    copy_label: TextSignal,
    /// Inline validation error under the paste-field (empty = none). Set on a
    /// failed format pre-check; cleared as the user edits. Reactive — updates
    /// without a rebuild.
    error: TextSignal,
    /// The paste-field's current NodeId, stashed each build so a failed-submit
    /// can re-focus it (a mouse click on Save otherwise moves focus to the
    /// button, dropping the caret). NodeIds change per rebuild, so this holds
    /// the latest live one.
    field_id: Rc<Cell<Option<NodeId>>>,
}

/// Validate a pasted Spotify client id without a network round-trip. A
/// client id is exactly 32 hexadecimal characters; this catches the common
/// paste mistakes (whitespace, a partial paste, a pasted URL, wrong length)
/// before we navigate to login. Returns the normalised (lowercased) id, or
/// an error message to show inline. True liveness is proven by the actual
/// Spotify login, which rejects an unregistered id with "INVALID_CLIENT".
fn validate_client_id(raw: &str) -> Result<String, &'static str> {
    let id = raw.trim();
    if id.is_empty() {
        return Err("Enter your Client ID.");
    }
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("That doesn't look like a Client ID — it should be 32 letters/numbers.");
    }
    Ok(id.to_ascii_lowercase())
}

impl SetupView {
    pub fn new(state: Rc<RefCell<AppState>>, icons: Rc<IconSet>, rebuild: Rc<Cell<bool>>) -> Self {
        Self {
            state,
            icons,
            rebuild,
            draft: Rc::new(RefCell::new(String::new())),
            copied: Signal::new(0.0),
            copy_label: TextSignal::new("Click to copy"),
            error: TextSignal::new(""),
            field_id: Rc::new(Cell::new(None)),
        }
    }

    pub fn build(&self, s: &mut Scene) {
        let state = self.state.clone();
        let draft = self.draft.clone();

        // Two save entry points (field Enter + button) share one path; each
        // needs its own clone of `self`-state captured into a 'static closure.
        let save_for_submit = {
            let me = self.clone_handle();
            let draft = draft.clone();
            move |ctx: &mut EventCtx| me.save(&draft.borrow(), ctx)
        };
        let save_for_click = {
            let me = self.clone_handle();
            let draft = draft.clone();
            move |ctx: &mut EventCtx| me.save(&draft.borrow(), ctx)
        };
        let copied = self.copied.clone();
        let copy_label = self.copy_label.clone();
        let error = self.error.clone();
        // Editing the field dismisses any stale validation error.
        let draft_change = {
            let draft = draft.clone();
            let error = self.error.clone();
            move |v: &str| {
                *draft.borrow_mut() = v.to_string();
                error.set("");
            }
        };

        s.col(())
            .fill()
            .rgba(tokens::BG[0], tokens::BG[1], tokens::BG[2], 1.0)
            .child(|root| {
                chrome::title_bar(root, &self.icons, "Opal");

                // Fade + slide the content in on view entry (`view_t` 0→1).
                let t = state.borrow().router.view_t.clone();
                let fade = Computed::new((t.clone(),), |(tt,)| tt.clamp(0.0, 1.0));
                let slide =
                    Computed::new((t.clone(),), |(tt,)| [0.0, (1.0 - tt.clamp(0.0, 1.0)) * 16.0]);

                root.col(())
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .center()
                    .gap(20.0)
                    .pos(slide)
                    .opacity_bind(fade)
                    .child(|c| {
                        // Logo mark beside the wordmark (static for now).
                        c.row(()).align(Align::Center).gap(tokens::SP_3).child(|r| {
                            self.icons.render_logo(r, 56.0);
                            r.text((), "Opal", tokens::TEXT_4XL).color(tokens::TEXT);
                        });
                        c.text((), "An unofficial Spotify desktop client.", tokens::TEXT_BASE)
                            .color(tokens::TEXT_DIM);

                        card(
                            c,
                            draft_change,
                            copied,
                            copy_label,
                            error,
                            self.field_id.clone(),
                            save_for_submit,
                            save_for_click,
                        );
                    });
            });
    }

    /// A cheap `Rc`-cloning handle of the view for capture into 'static
    /// event closures (the view itself isn't `Clone`).
    fn clone_handle(&self) -> SetupHandle {
        SetupHandle {
            state: self.state.clone(),
            rebuild: self.rebuild.clone(),
            error: self.error.clone(),
            field_id: self.field_id.clone(),
        }
    }
}

/// The capture-friendly slice of [`SetupView`] needed by the save closures.
#[derive(Clone)]
struct SetupHandle {
    state: Rc<RefCell<AppState>>,
    rebuild: Rc<Cell<bool>>,
    error: TextSignal,
    field_id: Rc<Cell<Option<NodeId>>>,
}

impl SetupHandle {
    fn save(&self, raw: &str, ctx: &mut EventCtx) {
        // Format pre-check before we navigate: a malformed id can't possibly
        // log in, so surface the problem inline rather than bouncing to a
        // dead login screen.
        let id = match validate_client_id(raw) {
            Ok(id) => id,
            Err(msg) => {
                self.error.set(msg);
                // A mouse click on Save moved focus to the button; put it
                // back on the field so the caret returns + they can fix it.
                if let Some(id) = self.field_id.get() {
                    ctx.tree.request_focus(id);
                }
                return;
            }
        };
        self.error.set("");
        let mut st = self.state.borrow_mut();
        st.prefs.data.spotify_client_id = Some(id);
        if let Err(e) = st.prefs.data.save() {
            log::warn!("saving client id failed: {e}");
        }
        // Reached Login *from* Setup → offer a Back affordance there.
        st.router.came_from_setup = true;
        st.router.go_view(View::Login, ctx.timeline, ctx.now);
        self.rebuild.set(true);
    }
}

/// The setup card: dashboard instructions + a client-id paste-field and
/// Save button.
#[allow(clippy::too_many_arguments)]
fn card(
    c: &mut Scene,
    on_change: impl Fn(&str) + 'static,
    copied: Signal<f32>,
    copy_label: TextSignal,
    error: TextSignal,
    field_id: Rc<Cell<Option<NodeId>>>,
    on_submit: impl Fn(&mut EventCtx) + 'static,
    on_click: impl Fn(&mut EventCtx) + 'static,
) {
    c.col(())
        .w_px(460.0)
        .pad(24.0)
        .gap(14.0)
        .rgba(1.0, 1.0, 1.0, 0.04)
        .radius(tokens::R_2XL)
        .border(1.0, [1.0, 1.0, 1.0, 0.08])
        .child(|card| {
            card.text((), "Set up your Spotify client id", tokens::TEXT_LG)
                .color(tokens::TEXT);
            card.text(
                (),
                "Opal needs a Spotify app of your own to sign in. It takes a minute:",
                tokens::TEXT_SM,
            )
            .color(tokens::TEXT_DIM);

            for line in INSTRUCTIONS {
                card.text((), *line, tokens::TEXT_SM).color(tokens::TEXT_DIM);
            }

            // The redirect URI must match the registered value exactly or
            // Spotify rejects the callback — call it out on its own line,
            // as a click-to-copy pill so the user can paste it verbatim.
            card.text((), "Redirect URI (must match exactly):", tokens::TEXT_SM)
                .color(tokens::TEXT_DIM);
            // Click-to-copy pill. On click it flashes green + flips the hint
            // to "Copied!" instantly, then resets after ~1.8s — driven by the
            // `copied` tween (set to 1.0, eased to 0.0) thresholded so the
            // feedback is a hard on/off (no fade), all reactive (no rebuild).
            const ON: f32 = 0.05;
            const GREEN: [f32; 4] = [0.42, 0.85, 0.52, 1.0];
            let copied_bg = copied.clone();
            let copied_fg = copied.clone();
            let copied_click = copied.clone();
            let label_drv = copy_label.clone();
            card.row(())
                .w(Len::Fill)
                .h_px(36.0)
                .pad_xy(12.0, 0.0)
                .gap(8.0)
                .align(Align::Center)
                .color(Computed::new((copied_bg,), |(t,)| {
                    if t > ON {
                        [0.20, 0.50, 0.30, 0.45]
                    } else {
                        [0.0, 0.0, 0.0, 0.30]
                    }
                }))
                .radius(tokens::R_LG)
                .border(1.0, [1.0, 1.0, 1.0, 0.10])
                .on_click(move |ctx: &mut EventCtx| {
                    ctx.tree.request_clipboard(SPOTIFY_REDIRECT_URI);
                    copied_click.set(1.0);
                    ctx.timeline.animate(
                        &copied_click,
                        0.0,
                        Curve::Linear,
                        Duration::from_millis(1800),
                        ctx.now,
                    );
                })
                .child(|r| {
                    r.text((), SPOTIFY_REDIRECT_URI, tokens::TEXT_SM)
                        .color(tokens::ACCENT);
                    // Single reactive label, right-aligned. Its colour bind
                    // doubles as the text driver: `Computed<String>` isn't
                    // expressible (Source is Copy-bound), so derive the label
                    // from `copied`'s threshold here as a side effect.
                    r.text_bound((), copy_label.clone(), tokens::TEXT_XS)
                        .push_end()
                        .color(Computed::new((copied_fg,), move |(t,)| {
                            let on = t > ON;
                            label_drv.set(if on { "Copied!" } else { "Click to copy" });
                            if on { GREEN } else { tokens::TEXT_DIM }
                        }));
                });

            let mut field = card.text_field((), "", tokens::TEXT_BASE);
            field
                .placeholder("Paste your Client ID")
                .w(Len::Fill)
                .h_px(44.0)
                .pad_xy(12.0, 0.0)
                .align(Align::Center)
                .rgba(0.0, 0.0, 0.0, 0.30)
                .radius(tokens::R_LG)
                .border(1.0, [1.0, 1.0, 1.0, 0.15])
                .justify(Justify::Start)
                .text_color(tokens::TEXT)
                .placeholder_color(tokens::TEXT_DIM)
                .on_change(on_change)
                .on_submit(on_submit);
            // Stash the live NodeId so a failed submit can re-focus the field.
            field_id.set(Some(field.id()));

            // Inline validation error (empty unless a bad id was submitted).
            // Reactive text — appears/clears without a rebuild.
            card.text_bound((), error, tokens::TEXT_SM)
                .color([0.93, 0.46, 0.46, 1.0]);

            card.row(())
                .w(Len::Fill)
                .h_px(44.0)
                .color(tokens::ACCENT)
                .hover_color(tokens::ACCENT_HOVER)
                .radius(tokens::R_LG)
                .center()
                .on_click(on_click)
                .child(|b| {
                    b.text((), "Save & continue", tokens::TEXT_BASE)
                        .color([1.0, 1.0, 1.0, 1.0]);
                });
        });
}

#[cfg(test)]
mod tests {
    use super::validate_client_id;

    #[test]
    fn accepts_valid_32_hex_id_and_normalises() {
        let id = "F6F1788623FA400EBAB54272BB3F515C";
        assert_eq!(
            validate_client_id(&format!("  {id}  ")).unwrap(),
            id.to_ascii_lowercase(),
            "trims + lowercases"
        );
    }

    #[test]
    fn rejects_blank_wrong_length_and_non_hex() {
        assert!(validate_client_id("   ").is_err(), "blank");
        assert!(validate_client_id("abc123").is_err(), "too short");
        assert!(
            validate_client_id("f6f1788623fa400ebab54272bb3f515cf").is_err(),
            "33 chars"
        );
        // 32 chars but contains non-hex (a pasted URL fragment, 'z', etc.).
        assert!(
            validate_client_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err(),
            "non-hex"
        );
    }
}
