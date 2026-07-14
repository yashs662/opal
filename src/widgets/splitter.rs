//! Drag-handle between panels. Mutates a caller-owned `Signal<f32>` to
//! resize the adjacent panel. Cursor flips to `EwResize` on hover (via
//! the lib's per-node cursor override).
//!
//! Renders a slim rounded pill, centred vertically, fading in on
//! hover/active-drag — invisible otherwise so it doesn't add chrome.
//!
//! Snap-collapse: dragging the cursor more than [`SNAP_HYSTERESIS`]
//! past the minimum width snaps the panel to fully hidden (`0`).
//! From hidden, the panel re-opens once the cursor has dragged at
//! least [`SNAP_OPEN`] in the opening direction — covers the
//! "accidentally collapsed" recovery path.

use std::cell::Cell;
use std::rc::Rc;

use opal_gfx::{Computed, CursorIcon, Len, Scene, Signal, deps};

/// Width of the (transparent) hit zone in logical pixels. Wide enough
/// to grab without precision, narrow enough to act as the panel gutter
/// itself (the parent row sets `.gap(0)` so this is the only spacing).
const HANDLE_W: f32 = 8.0;

/// Visible pill dimensions in logical pixels.
const PILL_W: f32 = 3.0;
const PILL_H: f32 = 48.0;
const PILL_RADIUS: f32 = 1.5;

/// How far past the minimum width the cursor must drag before the
/// panel snaps to fully collapsed (`width = 0`).
const SNAP_HYSTERESIS: f32 = 60.0;

/// How far in the opening direction the cursor must drag from a
/// collapsed panel before it snaps back open (jumps to `min`).
const SNAP_OPEN: f32 = 60.0;

/// Which side of the splitter the panel-being-resized lives on. The
/// splitter handle sits between two panels; this picks which one its
/// width signal controls and which cursor-delta direction "grows" it.
#[derive(Copy, Clone, Debug)]
pub enum PanelSide {
    /// Panel is to the **left** of the handle. Dragging right grows it.
    Left,
    /// Panel is to the **right** of the handle. Dragging right shrinks
    /// it (the cursor delta is inverted). Currently unused (the
    /// now-playing pane stopped being resizable) but kept — it's half
    /// the widget's API.
    #[allow(dead_code)]
    Right,
}

impl PanelSide {
    fn delta_sign(self) -> f32 {
        match self {
            PanelSide::Left => 1.0,
            PanelSide::Right => -1.0,
        }
    }
}

pub struct SplitterProps {
    pub name: &'static str,
    /// Panel width signal — clamped in `[min, max]` (or snapped to
    /// `collapsed`).
    pub width: Signal<f32>,
    pub side: PanelSide,
    pub min: f32,
    pub max: f32,
    /// Width the panel snaps to once the cursor drags past
    /// `min - SNAP_HYSTERESIS`. `0.0` = fully hide. A non-zero value
    /// keeps an icon-only stub visible (e.g. 80 px sidebar showing
    /// just playlist thumbs).
    pub collapsed: f32,
    /// Fired after every committed width change (one per cursor move).
    /// Hooked by the caller to mark user prefs dirty so the debounced
    /// save catches the new width.
    pub on_change: Rc<dyn Fn()>,
}

pub fn splitter(s: &mut Scene, props: SplitterProps) {
    let SplitterProps {
        name,
        width,
        side,
        min,
        max,
        collapsed,
        on_change,
    } = props;
    // Captured per-drag: width at press, refreshed on the press fire
    // (`DragCtx::delta == [0,0]`). All subsequent move fires read the
    // cursor offset from `DragCtx::start` and compute the new width
    // from the press-time snapshot — robust against per-event delta
    // accumulation drift.
    let start_w = Rc::new(Cell::new(0.0_f32));
    let start_w_for_drag = start_w.clone();
    let width_for_drag = width.clone();
    let on_change_for_drag = on_change;
    let sign = side.delta_sign();
    let hover = Signal::new(false);
    let pressed = Signal::new(false);
    let hover_for_color = hover.clone();
    let pressed_for_color = pressed.clone();
    let pill_color = Computed::new(deps!(hover_for_color, pressed_for_color), |(h, p)| {
        let a = if p {
            0.95
        } else if h {
            0.55
        } else {
            0.0
        };
        [1.0, 1.0, 1.0, a]
    });
    s.col(name)
        .w_px(HANDLE_W)
        .h(Len::Fill)
        .rgba(0.0, 0.0, 0.0, 0.0)
        .cursor(CursorIcon::EwResize)
        .on_hover(hover)
        .on_press(pressed)
        .center()
        .on_drag(move |d| {
            // Press fire (first event of every drag, fired by
            // `begin_drag_if_draggable` with delta zero). Snapshot the
            // current width — subsequent moves measure from this.
            if d.delta[0].abs() < f32::EPSILON && d.delta[1].abs() < f32::EPSILON {
                start_w_for_drag.set(width_for_drag.get());
                return;
            }
            // Cursor coords from the lib are in **physical** pixels;
            // panel widths live in logical px. Convert the cursor delta
            // through the display scale so a 100 logical-px mouse move
            // grows the panel by 100 logical px (not 100*scale).
            let scale = d.tree.scale().max(f32::EPSILON);
            let cursor_delta = (d.current[0] - d.start[0]) / scale;
            let candidate = start_w_for_drag.get() + sign * cursor_delta;
            let was_open = start_w_for_drag.get() > min - 0.5;
            let new_w = if was_open {
                // Snap-collapse when the cursor sits well past the
                // minimum (avoids a flickery snap at exactly `min`).
                if candidate < min - SNAP_HYSTERESIS {
                    collapsed
                } else {
                    candidate.clamp(min, max)
                }
            } else {
                // Reopening from collapsed: must drag at least
                // `SNAP_OPEN` past `collapsed` in the opening
                // direction before the panel re-emerges.
                if candidate > collapsed + SNAP_OPEN {
                    candidate.clamp(min, max)
                } else {
                    collapsed
                }
            };
            // Filter sub-pixel commits — each `set` triggers a
            // relayout, and a 500 Hz OS cursor stream with sub-px
            // deltas would relayout ~7× per displayed pixel for no
            // visual gain. 1 logical px == 1 commit boundary.
            if (new_w - width_for_drag.get()).abs() >= 1.0 {
                width_for_drag.set(new_w);
                on_change_for_drag();
            }
        })
        .child(|b| {
            b.rect(())
                .w_px(PILL_W)
                .h_px(PILL_H)
                .radius(PILL_RADIUS)
                .color(pill_color);
        });
}
