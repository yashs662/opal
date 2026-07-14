//! Settings panel — the *interior* of the settings modal.
//!
//! The modal shell (scrim, fade, input-blocking, click-to-dismiss) is the
//! engine's reusable [`opal_gfx::Overlay`]; this module only builds
//! what goes inside it. `home::build` calls `overlay.render(.., |panel|
//! settings::panel(panel, ..))`.
//!
//! The notable bit is [`toggle_switch`] — the animated on/off switch.

use std::rc::Rc;
use std::time::Duration;

use opal_gfx::{Align, Computed, Curve, Justify, Len, Overlay, Scene, Signal, TextBind, animated};

use crate::api::Profile;
use crate::audio_eq::GAIN_DB_MAX;
use crate::disk_cache::{self, CacheUsage};
use crate::model::{BackdropModel, CanvasModel, EqModel, SettingsModel};
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Colour of the album-art segment in the cache usage bar.
const CACHE_ART_COL: [f32; 4] = [0.36, 0.7, 0.95, 1.0];
/// Colour of the Canvas-video segment in the cache usage bar.
const CACHE_CANVAS_COL: [f32; 4] = [0.78, 0.5, 0.95, 1.0];
/// Colour of the API-JSON segment in the cache usage bar.
const CACHE_JSON_COL: [f32; 4] = [0.55, 0.82, 0.55, 1.0];
/// Colour of the streamed-audio segment in the cache usage bar.
const CACHE_AUDIO_COL: [f32; 4] = [0.95, 0.68, 0.38, 1.0];

// Animated toggle dimensions (logical px). The knob slides `TRAVEL` px
// between the two pad-inset ends of the track.
const TOGGLE_W: f32 = 44.0;
const TOGGLE_H: f32 = 24.0;
const TOGGLE_KNOB: f32 = 18.0;
const TOGGLE_PAD: f32 = 3.0;
const TOGGLE_TRAVEL: f32 = TOGGLE_W - TOGGLE_KNOB - 2.0 * TOGGLE_PAD;
/// Off-state track colour — a faint white so the switch reads as a
/// recessed pill before it lights up to the accent.
const TOGGLE_OFF: [f32; 4] = [1.0, 1.0, 1.0, 0.18];
/// Track + knob tween — snappy enough to feel responsive, slow enough to
/// read as motion.
const TOGGLE_MS: u64 = 160;

/// Widened from the old 420 so the 10-band EQ's vertical sliders sit in a
/// comfortable row (each band ≈ 44 px) without crowding the labels.
const PANEL_W: f32 = 500.0;
/// Capped panel height (logical px). The body scrolls past this, so the
/// modal never exceeds a typical window however many sections we add.
const PANEL_MAX_H: f32 = 640.0;
const SIGN_OUT_W: f32 = 116.0;

// EQ response graph — a Spotify-style filled curve with a draggable handle
// per band, a dB axis, and frequency labels. Widths are all responsive
// (`Fill`/`Auto`); the plot keeps one concrete height — a chart needs a
// definite value→pixel scale to map gains onto, and the handle/fill binds
// are computed in px against it.
const GRAPH_H: f32 = t::SP_32;
/// Sample rate the response preview is evaluated at (matches the sink).
const CURVE_FS: f64 = 44_100.0;
/// Draggable band-handle diameter (a small UI token, like an icon size).
const DOT: f32 = t::SP_2_5;
/// Graph plot background + 0 dB gridline colours.
const GRAPH_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.04];
const GRID_COL: [f32; 4] = [1.0, 1.0, 1.0, 0.16];

/// The settings modal — a [`Component`]. Reads its toggle/accent/cache
/// slices off the models directly; owns the [`Overlay`] render wrapper
/// (the overlay supplies the scrim, centring, fade and dismissal, so the
/// body here only styles + fills the panel). Costs nothing when closed.
pub struct SettingsPanel<'a> {
    pub settings: &'a SettingsModel,
    pub canvas: &'a CanvasModel,
    pub backdrop: &'a BackdropModel,
    pub profile: Option<&'a Profile>,
    pub icons: &'a Rc<IconSet>,
    /// Clear the stored token + return to Login.
    pub sign_out: Rc<dyn Fn()>,
    /// Persist after the canvas toggle flips (debounced prefs save).
    pub on_canvas_change: Rc<dyn Fn()>,
    /// Delete all cached files.
    pub on_clear_cache: Rc<dyn Fn()>,
    /// Open a folder picker to relocate the cache.
    pub on_change_cache_dir: Rc<dyn Fn()>,
    /// Current streaming-quality preference (selected chip).
    pub quality: crate::prefs::AudioQuality,
    /// Persist a new streaming-quality choice.
    pub on_quality: Rc<dyn Fn(crate::prefs::AudioQuality)>,
    /// Persist the "Normalize volume" toggle after it flips.
    pub on_normalize: Rc<dyn Fn()>,
    /// Equaliser slice — the slider signals + presets to bind/read.
    pub eq: &'a EqModel,
    /// A band slider was released → re-derive preset + persist.
    pub on_eq_commit: Rc<dyn Fn()>,
    /// The EQ enable toggle flipped → push to the sink + persist.
    pub on_eq_toggle: Rc<dyn Fn()>,
    /// Apply the preset at this index.
    pub on_eq_preset: Rc<dyn Fn(usize)>,
    /// Save the current sliders as a new custom preset.
    pub on_eq_save: Rc<dyn Fn()>,
    /// Expand/collapse the presets dropdown.
    pub on_eq_toggle_preset: Rc<dyn Fn()>,
    /// Delete the custom preset at an index.
    pub on_eq_delete: Rc<dyn Fn(usize)>,
    /// Begin inline-renaming the custom preset at an index.
    pub on_eq_rename_start: Rc<dyn Fn(usize)>,
    /// Commit the inline rename of the custom preset at an index.
    pub on_eq_rename_commit: Rc<dyn Fn(usize)>,
}

impl Component for SettingsPanel<'_> {
    fn view(&self, s: &mut Scene) {
        let icons = self.icons;
        // Measured on settings-open (a dir walk), not per build.
        let cache_usage = self.settings.cache_usage;
        let cache_path = disk_cache::root_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.settings.overlay.render(s, t::SCRIM, |host| {
            // Capped-height panel: the header stays pinned and the body
            // scrolls, so the modal never grows past the window however
            // many settings sections we add. Current content fits without
            // scrolling; future sections simply scroll into reach.
            // The panel is a glass node: it frosts whatever sits directly
            // behind it (the dimmed app), so a translucent tint reads as
            // frosted glass rather than a flat fill. `.layer()` promotes it
            // so the per-glass backdrop pass samples the content beneath.
            host.glass("settings_panel")
                .w_px(PANEL_W)
                .h_px(PANEL_MAX_H)
                .blur(28.0)
                .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.72)
                .radius(t::R_XL)
                .border(1.0, t::BORDER)
                .layer()
                // Right pad is trimmed (SP_6 → SP_2) so the scrolling body
                // extends into a right gutter; the scrollbar rides that gutter
                // instead of hugging the content. The header + body add their
                // own right pad back so nothing visible actually shifts.
                .pad_ltrb(t::SP_6, t::SP_6, t::SP_2, t::SP_4)
                .gap(t::SP_4)
                .child(|panel| {
                    // Pinned header.
                    header(panel, icons, self.settings.overlay.clone());
                    // Scrolling body — fills the remaining height; the thin
                    // auto-hiding scrollbar only shows when content overflows.
                    panel
                        .col(())
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .gap(t::SP_5)
                        .pad_ltrb(t::SP_0, t::SP_1, t::SP_6, t::SP_6)
                        .scroll_y()
                        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
                        .child(|body| {
                            setting_row(
                                body,
                                "Show canvas video",
                                "Looping artist visual in the now-playing pane",
                                &self.canvas.show,
                                &self.backdrop.accent,
                                self.on_canvas_change.clone(),
                            );
                            divider(body);
                            quality_row(
                                body,
                                self.quality,
                                &self.backdrop.accent,
                                self.on_quality.clone(),
                            );
                            setting_row(
                                body,
                                "Normalize volume",
                                "Match loudness across tracks + prevent clipping (next launch)",
                                &self.settings.normalize,
                                &self.backdrop.accent,
                                self.on_normalize.clone(),
                            );
                            divider(body);
                            eq_section(
                                body,
                                self.eq,
                                icons,
                                &self.backdrop.accent,
                                self.on_eq_toggle.clone(),
                                self.on_eq_commit.clone(),
                                PresetActions {
                                    apply: self.on_eq_preset.clone(),
                                    save: self.on_eq_save.clone(),
                                    toggle: self.on_eq_toggle_preset.clone(),
                                    delete: self.on_eq_delete.clone(),
                                    rename_start: self.on_eq_rename_start.clone(),
                                    rename_commit: self.on_eq_rename_commit.clone(),
                                },
                            );
                            divider(body);
                            cache_section(
                                body,
                                cache_usage,
                                &cache_path,
                                self.on_clear_cache.clone(),
                                self.on_change_cache_dir.clone(),
                            );
                            divider(body);
                            account(body, self.profile, self.sign_out.clone());
                        });
                });
        });
    }
}

/// Hairline divider between settings sections.
fn divider(s: &mut Scene) {
    s.rect(())
        .w(Len::Fill)
        .h_px(t::SP_PX)
        .rgba(1.0, 1.0, 1.0, 0.06);
}

/// The preset-dropdown emitter bundle — clone-cheap.
#[derive(Clone)]
pub struct PresetActions {
    pub apply: Rc<dyn Fn(usize)>,
    pub save: Rc<dyn Fn()>,
    pub toggle: Rc<dyn Fn()>,
    pub delete: Rc<dyn Fn(usize)>,
    pub rename_start: Rc<dyn Fn(usize)>,
    pub rename_commit: Rc<dyn Fn(usize)>,
}

/// The 10-band equaliser, Spotify-style: an enable toggle, a presets
/// dropdown, a filled response graph with a draggable handle per band
/// (over a dB axis and frequency labels), and a Reset. The body dims when
/// the EQ is off so it reads as inactive without disappearing.
fn eq_section(
    s: &mut Scene,
    eq: &EqModel,
    icons: &Rc<IconSet>,
    accent: &Signal<[f32; 4]>,
    on_toggle: Rc<dyn Fn()>,
    on_commit: Rc<dyn Fn()>,
    presets: PresetActions,
) {
    let enabled = eq.enabled.clone();
    let selected = eq.selected.clone();
    let open = eq.preset_open.get();
    let rename_idx = eq.rename_index.get();
    let rename_buf = eq.rename_buf.clone();
    // (name, custom) per preset — the dropdown shows delete/rename on customs.
    let entries: Vec<(Rc<str>, bool)> = eq
        .presets()
        .iter()
        .map(|p| (p.name.clone(), p.custom))
        .collect();
    let bands = eq.bands.clone();
    let shared = eq.shared();
    let icons = icons.clone();

    s.col(()).w(Len::Fill).gap(t::SP_3).child(move |c| {
        // Header + enable toggle.
        c.row(()).w(Len::Fill).align(Align::Center).child(|h| {
            h.col(()).gap(t::SP_0_5).child(|m| {
                m.text((), "Equalizer", 14.0).color(t::TEXT);
                m.text((), "10-band graphic EQ, applied live", t::TEXT_XS)
                    .color(t::TEXT_DIM);
            });
            h.row(())
                .push_end()
                .align(Align::Center)
                .child(|ctrl| toggle_switch(ctrl, &enabled, accent, on_toggle));
        });

        // Everything below dims when the EQ is off.
        let dim = Computed::new((enabled.clone(),), |(on,)| if on { 1.0 } else { 0.4 });
        c.col(())
            .w(Len::Fill)
            .gap(t::SP_4)
            .opacity_bind(dim)
            .child(move |body| {
                let reset = presets.apply.clone();
                preset_dropdown(
                    body, &icons, accent, &selected, &entries, open, rename_idx, rename_buf,
                    &presets,
                );
                eq_graph(body, &bands, shared, accent, on_commit);
                // Reset to flat (preset 0), right-aligned like Spotify's.
                body.row(()).w(Len::Fill).child(move |f| {
                    f.row(())
                        .push_end()
                        .h_px(t::SP_8)
                        .pad_xy(t::SP_4, 0.0)
                        .radius(t::R_FULL)
                        .border(1.0, t::BORDER)
                        .center()
                        .hover_color(t::BTN_HOVER)
                        .on_click(move |_| reset(0))
                        .child(|b| {
                            b.text((), "Reset", t::TEXT_SM).color(t::TEXT);
                        });
                });
            });
    });
}

/// The presets row: a "Presets" label and a dropdown whose button shows
/// the active preset (reactive off `selected`) and, when `open`, expands
/// an inline list of every preset plus "Save current…". Inline (accordion)
/// rather than a floating popup so it needs no portal/scrim: the toggle
/// rebuilds, and picking an item both applies and closes it. Custom presets
/// reveal rename (pen) + delete (trash) affordances on hover, and rename
/// swaps the row for an inline text field.
#[allow(clippy::too_many_arguments)]
fn preset_dropdown(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    accent: &Signal<[f32; 4]>,
    selected: &Signal<i32>,
    entries: &[(Rc<str>, bool)],
    open: bool,
    rename_idx: i32,
    rename_buf: Rc<std::cell::RefCell<String>>,
    actions: &PresetActions,
) {
    let entries = entries.to_vec();
    let label_names: Vec<Rc<str>> = entries.iter().map(|(n, _)| n.clone()).collect();
    let sel_now = selected.get();
    let icons = icons.clone();
    let actions = actions.clone();
    s.row(())
        .w(Len::Fill)
        .align(Align::Start)
        .gap(t::SP_3)
        .child(move |row| {
            row.row(()).h_px(t::SP_9).align(Align::Center).child(|l| {
                l.text((), "Presets", t::TEXT_SM).color(t::TEXT_DIM);
            });
            row.col(()).w(Len::Fill).gap(t::SP_1).child(move |dd| {
                // The button: current preset name + chevron.
                let label = TextBind::derived(selected.clone(), move |sel| {
                    usize::try_from(sel)
                        .ok()
                        .and_then(|i| label_names.get(i))
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Custom".to_string())
                });
                let ic = icons.clone();
                let toggle = actions.toggle.clone();
                dd.row(())
                    .w(Len::Fill)
                    .h_px(t::SP_9)
                    .pad_xy(t::SP_3, t::SP_0)
                    .align(Align::Center)
                    .radius(t::R_MD)
                    .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
                    .border(1.0, t::BORDER)
                    .hover_color(t::BTN_HOVER)
                    .on_click(move |_| toggle())
                    .child(move |b| {
                        b.text_bound((), label, t::TEXT_BASE).color(t::TEXT);
                        b.row(()).push_end().w_px(t::SP_5).center().child(|c| {
                            ic.render(c, Icon::ChevronDown, t::ICON_SM, t::TEXT_DIM);
                        });
                    });
                // The expanded list.
                if open {
                    dd.col(())
                        .w(Len::Fill)
                        .radius(t::R_MD)
                        .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
                        .border(1.0, t::BORDER)
                        .pad_xy(t::SP_0, t::SP_1)
                        .child(move |list| {
                            for (i, (name, custom)) in entries.iter().enumerate() {
                                if rename_idx == i as i32 {
                                    preset_rename_row(list, i, name, &icons, &rename_buf, &actions);
                                } else {
                                    preset_item_row(
                                        list,
                                        i,
                                        name,
                                        *custom,
                                        sel_now == i as i32,
                                        accent,
                                        &icons,
                                        &actions,
                                    );
                                }
                            }
                            list.rect(())
                                .w(Len::Fill)
                                .h_px(t::SP_PX)
                                .rgba(1.0, 1.0, 1.0, 0.08);
                            // "Save current" only makes sense when the shape
                            // differs from every built-in — snapshotting a
                            // built-in would just duplicate it. Disable it
                            // (dim, no hover, no click) while the selection is
                            // a built-in preset.
                            let on_builtin = sel_now >= 0
                                && entries
                                    .get(sel_now as usize)
                                    .map(|(_, custom)| !custom)
                                    .unwrap_or(false);
                            if on_builtin {
                                list.row(())
                                    .w(Len::Fill)
                                    .h_px(t::SP_8)
                                    .pad_xy(t::SP_3, t::SP_0)
                                    .align(Align::Center)
                                    .child(|r| {
                                        r.text((), "Save current\u{2026}", t::TEXT_SM).color([
                                            t::TEXT_DIM[0],
                                            t::TEXT_DIM[1],
                                            t::TEXT_DIM[2],
                                            0.4,
                                        ]);
                                    });
                            } else {
                                let save = actions.save.clone();
                                list.row(())
                                    .w(Len::Fill)
                                    .h_px(t::SP_8)
                                    .pad_xy(t::SP_3, t::SP_0)
                                    .align(Align::Center)
                                    .hover_color(t::HOVER_LIFT_SUBTLE)
                                    .on_click(move |_| save())
                                    .child(|r| {
                                        r.text((), "Save current\u{2026}", t::TEXT_SM)
                                            .color(t::TEXT_DIM);
                                    });
                            }
                        });
                }
            });
        });
}

/// One preset row: name + (for customs) hover-revealed rename/delete icons.
/// Clicking the row applies the preset; the icons sit on top so their
/// clicks don't fall through to it.
#[allow(clippy::too_many_arguments)]
fn preset_item_row(
    s: &mut Scene,
    i: usize,
    name: &str,
    custom: bool,
    is_sel: bool,
    accent: &Signal<[f32; 4]>,
    icons: &Rc<IconSet>,
    actions: &PresetActions,
) {
    let apply = actions.apply.clone();
    let hovered = Signal::new(false);
    let mut item = s.row(());
    item.w(Len::Fill)
        .h_px(t::SP_9)
        .pad_xy(t::SP_3, t::SP_0)
        .align(Align::Center)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .on_hover(hovered.clone())
        .on_click(move |_| apply(i));
    if is_sel {
        item.color(accent.clone());
    }
    let text_col: opal_gfx::Bind<[f32; 4]> = if is_sel {
        crate::widgets::color::accent_fg(accent).into()
    } else {
        t::TEXT.into()
    };
    let name = name.to_string();
    let icons = icons.clone();
    let del = actions.delete.clone();
    let ren = actions.rename_start.clone();
    item.child(move |r| {
        r.text((), &name, t::TEXT_SM).color(text_col);
        if custom {
            // Rename + delete, faded in on row hover (animated).
            let vis = animated(
                Computed::new((hovered.clone(),), |(h,)| if h { 1.0 } else { 0.0 }),
                Curve::EaseInOut,
                Duration::from_millis(120),
            );
            r.row(())
                .push_end()
                .gap(t::SP_1)
                .align(Align::Center)
                .opacity_bind(vis)
                .child(move |g| {
                    let ren = ren.clone();
                    g.row(())
                        .w_px(t::SP_6)
                        .h_px(t::SP_6)
                        .center()
                        .radius(t::R_SM)
                        .cursor(opal_gfx::CursorIcon::Pointer)
                        .hover_color(t::BTN_HOVER)
                        .on_click(move |_| ren(i))
                        .child(|c| icons.render(c, Icon::Pen, t::ICON_SM, t::TEXT_DIM));
                    let del = del.clone();
                    g.row(())
                        .w_px(t::SP_6)
                        .h_px(t::SP_6)
                        .center()
                        .radius(t::R_SM)
                        .cursor(opal_gfx::CursorIcon::Pointer)
                        .hover_color(t::BTN_HOVER)
                        .on_click(move |_| del(i))
                        .child(|c| icons.render(c, Icon::Trash, t::ICON_SM, [0.92, 0.4, 0.4, 1.0]));
                });
        }
    });
}

/// The rename variant of a preset row: an inline text field seeded with the
/// current name (commit on Enter or the check button).
fn preset_rename_row(
    s: &mut Scene,
    i: usize,
    name: &str,
    icons: &Rc<IconSet>,
    rename_buf: &Rc<std::cell::RefCell<String>>,
    actions: &PresetActions,
) {
    let commit = actions.rename_commit.clone();
    let commit_btn = commit.clone();
    let buf = rename_buf.clone();
    let name = name.to_string();
    let icons = icons.clone();
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_9)
        .pad_xy(t::SP_2, t::SP_0)
        .gap(t::SP_1)
        .align(Align::Center)
        .child(move |r| {
            let mut field = r.text_field((), name.as_str(), t::TEXT_SM);
            field
                .w(Len::Fill)
                .h_px(t::SP_7)
                .pad_xy(t::SP_2, t::SP_0)
                .align(Align::Center)
                .justify(Justify::Start)
                .rgba(0.0, 0.0, 0.0, 0.30)
                .radius(t::R_SM)
                .border(1.0, t::BORDER)
                .text_color(t::TEXT)
                .placeholder_color(t::TEXT_DIM)
                .on_change(move |s| *buf.borrow_mut() = s.to_string())
                .on_submit(move |_| commit(i));
            r.row(())
                .push_end()
                .w_px(t::SP_7)
                .h_px(t::SP_7)
                .center()
                .radius(t::R_SM)
                .cursor(opal_gfx::CursorIcon::Pointer)
                .hover_color(t::BTN_HOVER)
                .on_click(move |_| commit_btn(i))
                .child(|c| icons.render(c, Icon::Check, t::ICON_SM, t::TEXT));
        });
}

/// The response graph: a translucent-accent filled magnitude-response
/// curve with a draggable handle per band, a dB axis (auto-width), and
/// frequency labels — all widths responsive. The fill and the handles are
/// reactive off two 5-band group `Computed`s (one `Computed` can't take
/// all ten as deps), so a preset apply tweens them and a drag tracks with
/// no rebuild. The one fixed dimension is the plot height, which the
/// gain→pixel binds map onto.
fn eq_graph(
    s: &mut Scene,
    bands: &[Signal<f32>; crate::audio_eq::NUM_BANDS],
    shared: std::sync::Arc<crate::audio_eq::EqShared>,
    accent: &Signal<[f32; 4]>,
    on_commit: Rc<dyn Fn()>,
) {
    use crate::audio_eq::{BAND_FREQS, BAND_LABELS, NUM_BANDS};
    let g_lo = band_group(bands, 0);
    let g_hi = band_group(bands, 5);
    // Accent tint for the curve effect: the shader fills below it at this
    // alpha and strokes the curve itself at full alpha.
    let fill_col = Computed::new((accent.clone(),), |(a,)| [a[0], a[1], a[2], 0.28]);
    // The curve's control points: the response fraction (0..1 from the
    // bottom) at each band's centre frequency, so the spline passes through
    // the handles. Reactive → the shader re-strokes as bands tween/drag.
    let points = Computed::new((g_lo.clone(), g_hi.clone()), |(lo, hi)| {
        std::array::from_fn::<f32, NUM_BANDS, _>(|i| response_frac(lo, hi, BAND_FREQS[i] as f64))
    });
    let bands = bands.clone();
    let accent = accent.clone();

    s.row(())
        .w(Len::Fill)
        .align(Align::Start)
        .gap(t::SP_2)
        .child(move |gr| {
            // dB axis — auto-width (sizes to its labels), right-aligned.
            gr.col(())
                .w(Len::Auto)
                .h_px(GRAPH_H)
                .justify(Justify::SpaceBetween)
                .align(Align::End)
                .child(|ax| {
                    ax.text((), "+12dB", t::TEXT_XS).color(t::TEXT_DIM);
                    ax.text((), "0", t::TEXT_XS).color(t::TEXT_DIM);
                    ax.text((), "-12dB", t::TEXT_XS).color(t::TEXT_DIM);
                });
            // Plot + frequency labels fill the remaining width.
            gr.col(()).w(Len::Fill).gap(t::SP_1).child(move |right| {
                right
                    .col(())
                    .w(Len::Fill)
                    .h_px(GRAPH_H)
                    .radius(t::R_SM)
                    .rgba(GRAPH_BG[0], GRAPH_BG[1], GRAPH_BG[2], GRAPH_BG[3])
                    .overflow(opal_gfx::Overflow::Hidden, opal_gfx::Overflow::Hidden)
                    .child(move |plot| {
                        // The response curve — a GPU-shaded, anti-aliased filled
                        // spline through the band points (one `curve` effect node,
                        // reactive off `points`), replacing the old bar fill.
                        plot.curve(())
                            .abs(0.0, 0.0)
                            .w(Len::Fill)
                            .h_px(GRAPH_H)
                            .color(fill_col)
                            .effect_data_bind(points);
                        // 0 dB gridline across the vertical centre.
                        plot.rect(())
                            .abs(0.0, GRAPH_H / 2.0 - 0.5)
                            .w(Len::Fill)
                            .h_px(1.0)
                            .rgba(GRID_COL[0], GRID_COL[1], GRID_COL[2], GRID_COL[3]);
                        // Draggable band handles on top.
                        plot.row(())
                            .abs(0.0, 0.0)
                            .w(Len::Fill)
                            .h_px(GRAPH_H)
                            .child(move |cols| {
                                for (i, band) in bands.into_iter().enumerate() {
                                    band_handle(
                                        cols,
                                        i,
                                        g_lo.clone(),
                                        g_hi.clone(),
                                        band,
                                        shared.clone(),
                                        &accent,
                                        on_commit.clone(),
                                    );
                                }
                            });
                    });
                // Frequency labels, one per handle column (aligned by matching
                // the handle row's equal Fill columns).
                right.row(()).w(Len::Fill).child(|fl| {
                    for label in BAND_LABELS.iter() {
                        fl.col(()).w(Len::Fill).center().child(|c| {
                            c.text((), *label, t::TEXT_XS).color(t::TEXT_DIM);
                        });
                    }
                });
            });
        });
}

/// A draggable band handle: a dot sitting on the curve at its band's
/// centre frequency (positioned by the reactive response, so it rides the
/// fill), inside a full-height column that captures the drag. Dragging
/// maps the cursor to a whole-dB gain and writes the band signal + shared
/// surface; release commits.
#[allow(clippy::too_many_arguments)]
fn band_handle(
    s: &mut Scene,
    index: usize,
    g_lo: Computed<[f32; 5]>,
    g_hi: Computed<[f32; 5]>,
    band: Signal<f32>,
    shared: std::sync::Arc<crate::audio_eq::EqShared>,
    accent: &Signal<[f32; 4]>,
    on_commit: Rc<dyn Fn()>,
) {
    let freq = crate::audio_eq::BAND_FREQS[index] as f64;
    // Dot top offset (px): the response height at this band, centred on the
    // dot, so the dot sits on the curve.
    let dot_top = Computed::new((g_lo, g_hi), move |(lo, hi)| {
        let y = (1.0 - response_frac(lo, hi, freq)) * GRAPH_H;
        (y - DOT / 2.0).clamp(0.0, GRAPH_H - DOT)
    });
    let band_for_label = band.clone();
    let sig = band;
    let sh = shared;
    let commit = on_commit;
    let acc = accent.clone();
    // Hover-revealed dB readout so the exact gain isn't opaque. Fades in as
    // the cursor enters the band's column.
    let hovered = Signal::new(false);
    let label_vis = animated(
        Computed::new((hovered.clone(),), |(h,)| if h { 1.0 } else { 0.0 }),
        Curve::EaseInOut,
        Duration::from_millis(120),
    );
    let db_label = TextBind::derived(band_for_label, |db: f32| {
        if db.abs() < 0.5 {
            "0 dB".to_string()
        } else {
            format!("{:+.0} dB", db.round())
        }
    });
    s.col(())
        .w(Len::Fill)
        .h_px(GRAPH_H)
        .align(Align::Center)
        .cursor(opal_gfx::CursorIcon::Pointer)
        .on_hover(hovered)
        .on_drag(move |ctx| {
            // Top = +MAX, bottom = −MAX; fraction physical/physical so scale
            // cancels. Snap to whole dB.
            let frac = ((ctx.current[1] - ctx.rect[1]) / ctx.rect[3]).clamp(0.0, 1.0);
            let db = (GAIN_DB_MAX * (1.0 - 2.0 * frac)).round();
            sig.set(db);
            sh.set_band(index, db);
        })
        .on_drag_end(move |_| commit())
        .child(move |col| {
            // Transparent spacer pushes the dot down to the curve.
            col.rect(())
                .w_px(1.0)
                .height_px_bind(dot_top.clone())
                .rgba(0.0, 0.0, 0.0, 0.0);
            col.rect(())
                .w_px(DOT)
                .h_px(DOT)
                .radius(t::R_FULL)
                .color(acc.clone());
            // dB readout pinned at the top of the column, revealed on hover.
            col.row(())
                .abs(0.0, t::SP_0_5)
                .w(Len::Fill)
                .center()
                .opacity_bind(label_vis.clone())
                .child(move |lbl| {
                    lbl.row(())
                        .pad_xy(t::SP_1, t::SP_0_5)
                        .radius(t::R_SM)
                        .rgba(0.0, 0.0, 0.0, 0.55)
                        .center()
                        .child(move |p| {
                            p.text_bound((), db_label, t::TEXT_XS).color(t::TEXT);
                        });
                });
        });
}

/// A `Computed<[f32;5]>` over five consecutive band signals starting at
/// `base` — the group deps for the response `Computed`s (which can't take
/// all ten bands at once).
fn band_group(
    bands: &[Signal<f32>; crate::audio_eq::NUM_BANDS],
    base: usize,
) -> Computed<[f32; 5]> {
    Computed::new(
        (
            bands[base].clone(),
            bands[base + 1].clone(),
            bands[base + 2].clone(),
            bands[base + 3].clone(),
            bands[base + 4].clone(),
        ),
        |(a, b, c, d, e)| [a, b, c, d, e],
    )
}

/// Combined response at `freq` as a 0..1 fraction of the graph height
/// (0 = −12 dB at the bottom, 0.5 = 0 dB, 1 = +12 dB at the top).
fn response_frac(lo: [f32; 5], hi: [f32; 5], freq: f64) -> f32 {
    let gains = [
        lo[0], lo[1], lo[2], lo[3], lo[4], hi[0], hi[1], hi[2], hi[3], hi[4],
    ];
    let db = crate::audio_eq::response_db(&gains, freq, CURVE_FS) as f32;
    ((db + GAIN_DB_MAX) / (2.0 * GAIN_DB_MAX)).clamp(0.0, 1.0)
}

/// Streaming-quality picker: three chips (96 / 160 / 320 kbps), the
/// active one accent-filled. Bitrate is baked into the librespot player
/// at session start, so a change applies from the next launch — the
/// caption says so rather than pretending it's instant.
fn quality_row(
    s: &mut Scene,
    current: crate::prefs::AudioQuality,
    accent: &Signal<[f32; 4]>,
    on_quality: Rc<dyn Fn(crate::prefs::AudioQuality)>,
) {
    use crate::prefs::AudioQuality as Q;
    s.col(()).w(Len::Fill).gap(t::SP_2).child(move |c| {
        c.col(()).gap(t::SP_0_5).child(|m| {
            m.text((), "Streaming quality", 14.0).color(t::TEXT);
            m.text((), "Applies on next launch", t::TEXT_XS)
                .color(t::TEXT_DIM);
        });
        c.row(()).gap(t::SP_2).child(move |row| {
            for (q, label) in [
                (Q::Low, "Low 96k"),
                (Q::Normal, "Normal 160k"),
                (Q::High, "High 320k"),
            ] {
                let selected = q == current;
                let mut chip = row.row(());
                chip.h_px(t::CHIP_H)
                    .pad_xy(t::SP_3_5, t::SP_0)
                    .center()
                    .radius(t::R_FULL);
                if selected {
                    chip.color(accent.clone()).child(|x| {
                        x.text((), label, 13.0)
                            .color(crate::widgets::color::accent_fg(accent));
                    });
                } else {
                    let on_quality = on_quality.clone();
                    chip.color(t::PANEL_HI)
                        .hover_opacity(0.8)
                        .on_click(move |_| on_quality(q))
                        .child(|x| {
                            x.text((), label, 13.0).color(t::TEXT);
                        });
                }
            }
        });
    });
}

/// Human-readable byte size (e.g. `1.2 GB`, `340 MB`, `12 KB`).
fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = b as f64;
    if f >= GB {
        format!("{:.2} GB", f / GB)
    } else if f >= MB {
        format!("{:.1} MB", f / MB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{b} B")
    }
}

/// Cache management: a usage-breakdown bar (album-art/Canvas vs API JSON),
/// the on-disk location with a relocate button, and a clear-cache button.
fn cache_section(
    s: &mut Scene,
    usage: CacheUsage,
    path: &str,
    on_clear: Rc<dyn Fn()>,
    on_change_dir: Rc<dyn Fn()>,
) {
    let total = usage.total();
    let frac = |b: u64| {
        if total > 0 {
            b as f32 / total as f32
        } else {
            0.0
        }
    };
    // Non-zero segments in draw order, for the proportional bar. End caps
    // are rounded by rounding the first segment's left + last's right.
    let segments: Vec<(f32, [f32; 4])> = [
        (frac(usage.audio), CACHE_AUDIO_COL),
        (frac(usage.art), CACHE_ART_COL),
        (frac(usage.canvas), CACHE_CANVAS_COL),
        (frac(usage.json), CACHE_JSON_COL),
    ]
    .into_iter()
    .filter(|(f, _)| *f > 0.0)
    .collect();
    let total_label = fmt_bytes(total);
    let audio_label = format!("Audio  {}", fmt_bytes(usage.audio));
    let art_label = format!("Album art  {}", fmt_bytes(usage.art));
    let canvas_label = format!("Canvas  {}", fmt_bytes(usage.canvas));
    let json_label = format!("Metadata  {}", fmt_bytes(usage.json));
    let path = path.to_string();
    s.col(()).w(Len::Fill).gap(t::SP_2).child(move |c| {
        c.row(()).w(Len::Fill).align(Align::Center).child(|h| {
            h.text((), "Storage", t::TEXT_SM).color(t::TEXT_DIM);
            h.row(()).push_end().child(|e| {
                e.text((), &total_label, t::TEXT_SM).color(t::TEXT);
            });
        });
        // Proportional usage bar. Coloured segments fill it by each
        // category's share; the rounded track clips them (rounded overflow
        // clipping), so the whole bar reads as a clean pill with rounded
        // caps regardless of how thin the end segment is.
        c.row(())
            .w(Len::Fill)
            .h_px(t::SP_2)
            .radius(t::R_FULL)
            .rgba(1.0, 1.0, 1.0, 0.08)
            .overflow(opal_gfx::Overflow::Hidden, opal_gfx::Overflow::Hidden)
            .child(move |bar| {
                for (f, col) in &segments {
                    bar.rect(()).w(Len::Pct(*f)).h(Len::Fill).color(*col);
                }
            });
        // Legend — a 2×2 grid (two rows of two) so four categories fit
        // the panel width without the last one running off the edge.
        // Each cell fills half the row so the columns line up.
        c.col(()).w(Len::Fill).gap(t::SP_1_5).child(move |lg| {
            lg.row(()).w(Len::Fill).gap(t::SP_2).child(|r| {
                legend_dot(r, CACHE_AUDIO_COL, &audio_label);
                legend_dot(r, CACHE_ART_COL, &art_label);
            });
            lg.row(()).w(Len::Fill).gap(t::SP_2).child(|r| {
                legend_dot(r, CACHE_CANVAS_COL, &canvas_label);
                legend_dot(r, CACHE_JSON_COL, &json_label);
            });
        });
        // Location + relocate.
        c.row(())
            .w(Len::Fill)
            .align(Align::Center)
            .gap(t::SP_2)
            .child(move |loc| {
                loc.col(()).child(|p| {
                    p.text((), "Location", t::TEXT_XS).color(t::TEXT_DIM);
                    p.text((), &path, t::TEXT_XS)
                        .color(t::TEXT)
                        .max_width_px(240.0);
                });
                loc.row(())
                    .push_end()
                    .h_px(t::SP_8)
                    .pad_xy(t::SP_3, 0.0)
                    .radius(t::R_FULL)
                    .border(1.0, t::BORDER)
                    .center()
                    .hover_color(t::BTN_HOVER)
                    .on_click(move |_| on_change_dir())
                    .child(|b| {
                        b.text((), "Change\u{2026}", t::TEXT_SM).color(t::TEXT);
                    });
            });
        // Clear — full width to match the section.
        c.row(())
            .w(Len::Fill)
            .h_px(t::SP_9)
            .radius(t::R_FULL)
            .border(1.0, t::BORDER)
            .center()
            .hover_color(t::BTN_HOVER)
            .on_click(move |_| on_clear())
            .child(|b| {
                b.text((), "Clear cache", t::TEXT_SM).color(t::TEXT);
            });
    });
}

/// A small coloured dot + label, for the cache-bar legend.
/// One legend cell — a colour dot + label. Fills half its row so the two
/// columns of the 2×2 grid line up regardless of label width.
fn legend_dot(s: &mut Scene, color: [f32; 4], label: &str) {
    s.row(())
        .w(Len::Fill)
        .align(Align::Center)
        .gap(t::SP_1)
        .child(|d| {
            d.rect(())
                .w_px(t::SP_2)
                .h_px(t::SP_2)
                .radius(t::R_FULL)
                .color(color);
            d.text((), label, t::TEXT_XS)
                .color(t::TEXT_DIM)
                .max_width_px(150.0);
        });
}

fn header(s: &mut Scene, icons: &IconSet, overlay: Overlay) {
    s.row(())
        .w(Len::Fill)
        // Match the body's extra right pad so the close button keeps its
        // original position after the panel's right pad was trimmed for the
        // scrollbar gutter.
        .pad_ltrb(t::SP_0, t::SP_0, t::SP_4, t::SP_0)
        .align(Align::Center)
        .child(|h| {
            h.text((), "Settings", t::TEXT_XL).color(t::TEXT);
            h.row(())
                .push_end()
                .w_px(t::SP_8)
                .h_px(t::SP_8)
                .center()
                .hover_opacity(0.7)
                .on_click(move |ctx| overlay.close(ctx.timeline, ctx.now))
                .child(|c| {
                    icons.render(c, Icon::Close, t::ICON_MD, t::TEXT_DIM);
                });
        });
}

/// A labelled row with a trailing animated toggle switch.
fn setting_row(
    s: &mut Scene,
    title: &str,
    subtitle: &str,
    state: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
    on_change: Rc<dyn Fn()>,
) {
    s.row(()).w(Len::Fill).align(Align::Center).child(|r| {
        r.col(()).gap(t::SP_0_5).child(|c| {
            c.text((), title, t::TEXT_BASE).color(t::TEXT);
            c.text((), subtitle, t::TEXT_XS).color(t::TEXT_DIM);
        });
        r.row(())
            .push_end()
            .align(Align::Center)
            .child(|ctrl| toggle_switch(ctrl, state, accent, on_change));
    });
}

/// The animated on/off switch. A `knob_t` signal (0..=TRAVEL px) is
/// **seeded to the current state** at build so opening the popup shows
/// the right position instantly — no spurious mount animation. Clicking
/// flips the bound `state` and tweens `knob_t` via the timeline; the
/// knob (spacer-width bind) and track colour (`Computed` over `knob_t`)
/// both follow, so the slide + colour fade are one smooth motion with no
/// scene rebuild. The lib bubbles a click on the knob up to this handler.
fn toggle_switch(
    s: &mut Scene,
    state: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
    on_change: Rc<dyn Fn()>,
) {
    let knob_t = Signal::new(if state.get() { TOGGLE_TRAVEL } else { 0.0 });
    let track_col = Computed::new((knob_t.clone(), accent.clone()), |(x, acc)| {
        let f = (x / TOGGLE_TRAVEL).clamp(0.0, 1.0);
        lerp4(TOGGLE_OFF, acc, f)
    });
    let st = state.clone();
    let kt = knob_t.clone();
    s.row(())
        .w_px(TOGGLE_W)
        .h_px(TOGGLE_H)
        .radius(t::R_FULL)
        .color(track_col)
        .align(Align::Center)
        .pad_xy(TOGGLE_PAD, 0.0)
        .on_click(move |ctx| {
            let now_on = !st.get();
            st.set(now_on);
            on_change();
            let target = if now_on { TOGGLE_TRAVEL } else { 0.0 };
            ctx.timeline.animate(
                &kt,
                target,
                Curve::EaseInOut,
                Duration::from_millis(TOGGLE_MS),
                ctx.now,
            );
        })
        .child(|tr| {
            // Spacer whose width tracks `knob_t` (0 → TRAVEL), pushing the
            // knob from the left end to the right as the tween advances.
            tr.rect(())
                .width_px_bind(knob_t.clone())
                .h_px(1.0)
                .rgba(0.0, 0.0, 0.0, 0.0);
            tr.rect(())
                .w_px(TOGGLE_KNOB)
                .h_px(TOGGLE_KNOB)
                .radius(t::R_FULL)
                .rgba(1.0, 1.0, 1.0, 1.0);
        });
}

/// Component-wise linear interpolation between two RGBA colours.
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

fn account(s: &mut Scene, profile: Option<&Profile>, sign_out: Rc<dyn Fn()>) {
    let name = profile
        .map(|p| p.display_name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("Spotify account");
    s.col(()).w(Len::Fill).gap(t::SP_2).child(|acc| {
        acc.text((), "Account", t::TEXT_SM).color(t::TEXT_DIM);
        // Name on the left, Sign out pushed to the right edge, both
        // vertically centred on one full-width row.
        acc.row(()).w(Len::Fill).align(Align::Center).child(|r| {
            r.text((), name, t::TEXT_BASE).color(t::TEXT);
            r.row(())
                .push_end()
                .w_px(SIGN_OUT_W)
                .h_px(t::SP_9)
                .radius(t::R_FULL)
                .border(1.0, t::BORDER)
                .center()
                .hover_color(t::BTN_HOVER)
                .on_click(move |_| sign_out())
                .child(|b| {
                    b.text((), "Sign out", t::TEXT_SM).color(t::TEXT);
                });
        });
    });
}
