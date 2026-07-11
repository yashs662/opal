//! Design tokens for the whole UI — every padding, gap, height, radius,
//! font size, and colour referenced anywhere in the views resolves
//! through here. Anchoring on a single scale (Tailwind's 4 px grid)
//! means changes line up with the spacing system instead of drifting
//! into arbitrary one-off pixel values, and that adjacent elements
//! share the same rhythm even when laid out independently.
//!
//! Names mirror Tailwind so anyone with that mental model gets it for
//! free (`SP_4 == 4 * 4 px == p-4`). Component-shaped aliases
//! (`ROW_H_LG`, `THUMB_MD`, etc.) sit on top of the raw scale so
//! callsites read semantically rather than as raw px.
//!
//! ## Adding a new constant
//! 1. If it's a length, derive from `SP_*`. Don't add raw px.
//! 2. If it's a colour, add it next to its peers in the **Palette**
//!    section. Keep alpha at the end (`rgba` tuple order).
//! 3. If you reach for a value that doesn't exist on the scale, your
//!    layout is wrong, not the scale.
//!
//! `dead_code` is allowed module-wide on purpose: a design system keeps
//! the *whole* scale defined (every step + semantic alias) so a layout
//! can reach for the right token without re-deriving it — unused steps
//! are the palette, not rot.
#![allow(dead_code)]

// ============================================================================
// Spacing scale (Tailwind 4 px grid)
// ============================================================================
//
// Every token is `BASE * <multiplier>`, where `<multiplier>` matches
// the Tailwind class number (`p-4` → multiplier 4 → `SP_4`). Change
// `BASE` and the whole UI rescales together — there are no raw px
// constants to chase. The single odd one out is `SP_PX`, the literal
// one-px hairline used for borders and dividers.
//
// **Adding a token:** drop a `NAME => multiplier` line into the list.
// Don't write out the product — let the macro compute it so the value
// can't drift from the scale.

const BASE: f32 = 4.0;

macro_rules! spacing_scale {
    ($($name:ident => $mul:expr),* $(,)?) => {
        $( pub const $name: f32 = BASE * $mul; )*
    };
}

pub const SP_PX: f32 = 1.0;
spacing_scale! {
    SP_0   => 0.0,
    SP_0_5 => 0.5,
    SP_1   => 1.0,
    SP_1_5 => 1.5,
    SP_2   => 2.0,
    SP_2_5 => 2.5,
    SP_3   => 3.0,
    SP_3_5 => 3.5,
    SP_4   => 4.0,
    SP_4_5 => 4.5,
    SP_5   => 5.0,
    SP_6   => 6.0,
    SP_7   => 7.0,
    SP_8   => 8.0,
    SP_9   => 9.0,
    SP_10  => 10.0,
    SP_11  => 11.0,
    SP_12  => 12.0,
    SP_14  => 14.0,
    SP_16  => 16.0,
    SP_20  => 20.0,
    SP_24  => 24.0,
    SP_28  => 28.0,
    SP_32  => 32.0,
    SP_40  => 40.0,
    SP_44  => 44.0,
    SP_48  => 48.0,
    SP_56  => 56.0,
    SP_64  => 64.0,
    SP_80  => 80.0,
}

// ============================================================================
// Border radius
// ============================================================================

pub const R_NONE: f32 = 0.0;
pub const R_SM: f32 = 4.0;
pub const R_MD: f32 = 6.0;
pub const R_LG: f32 = 8.0;
pub const R_XL: f32 = 12.0;
pub const R_2XL: f32 = 16.0;
pub const R_3XL: f32 = 24.0;
/// For pill / circle shapes — pass instead of `h/2`. The lib clamps to
/// `min(w, h)/2` internally, so any value larger than half the smallest
/// dimension yields a perfect pill regardless of size.
pub const R_FULL: f32 = 9999.0;

// ============================================================================
// Font sizes
// ============================================================================

pub const TEXT_XS: f32 = 11.0;
pub const TEXT_SM: f32 = 13.0;
pub const TEXT_BASE: f32 = 14.0;
pub const TEXT_LG: f32 = 18.0;
pub const TEXT_XL: f32 = 20.0;
pub const TEXT_2XL: f32 = 24.0;
pub const TEXT_3XL: f32 = 30.0;
pub const TEXT_4XL: f32 = 36.0;

// ============================================================================
// Component-shaped sizes
//
// All derived from `SP_*` so the spacing rhythm survives. Callsites
// read semantically (`ROW_H_LG`) instead of as raw px.
// ============================================================================

/// Standard row heights — `MD` = condensed list, `LG` = sidebar header.
pub const ROW_H_SM: f32 = SP_8;
pub const ROW_H_MD: f32 = SP_10;
pub const ROW_H_LG: f32 = SP_12;

/// Pill heights for filter chips + transport-style buttons.
pub const CHIP_H: f32 = SP_8;
pub const BTN_H_SM: f32 = SP_8;
pub const BTN_H_MD: f32 = SP_9;
pub const BTN_H_LG: f32 = SP_12;

/// Icon sizes (logical px). Off-scale values (`MD` = 18) intentionally
/// kept because icons rasterize at a fixed atlas size and the visual
/// weight needs to match the surrounding text — strict 4 px grid
/// produces icons that read too small at 16 or too large at 20.
pub const ICON_XS: f32 = SP_3_5;
pub const ICON_SM: f32 = SP_4;
pub const ICON_MD: f32 = SP_4_5;
pub const ICON_LG: f32 = SP_5;
pub const ICON_XL: f32 = SP_6;

/// Album-art thumb sizes.
pub const THUMB_SM: f32 = SP_10;
pub const THUMB_MD: f32 = SP_11;
pub const THUMB_LG: f32 = SP_12;
pub const THUMB_XL: f32 = SP_20;
pub const THUMB_2XL: f32 = SP_40;

/// Tile / card dimensions for the main pane grid.
pub const TILE_W: f32 = SP_44;
pub const TILE_THUMB: f32 = SP_40;
pub const TILE_TEXT_MAX: f32 = SP_40;

/// Top-bar + player-bar shell heights — the two fixed-height frames
/// that wrap the resizable middle region.
pub const PLAYER_H: f32 = SP_20;

// ----------------------------------------------------------------------------
// Sidebar widths
//
// The sidebar nests **two** padded containers around the thumb, so the
// fit-width is the sum of BOTH paddings on each side, not just one:
//
// Adjust the same arithmetic in `home::collapsed_text_width` if you
// change any of these — that helper subtracts the same chrome from
// `sidebar_w` to find how much text-col room is left in expanded mode.
// ----------------------------------------------------------------------------

/// Horizontal chrome on each side of the thumb: outer scroll-col
/// padding plus inner playlist-row padding. Times two = the width that
/// the thumb-only layout needs on top of `THUMB_LG`.
pub const SIDEBAR_ROW_PAD_X: f32 = SP_1_5 + SP_1_5;

/// Snug fit for thumb-only mode: thumb + nested padding on both sides.
pub const SIDEBAR_COLLAPSED: f32 = SIDEBAR_ROW_PAD_X + THUMB_LG + SIDEBAR_ROW_PAD_X;

/// Spacer (replaces flex `gap`) between thumb and text-col in the
/// expanded playlist row. Reactive — zero when collapsed so the row
/// shrinks to exactly `SIDEBAR_COLLAPSED`.
pub const SIDEBAR_TEXT_SPACER: f32 = SP_2;

/// Total non-text horizontal chrome in an expanded playlist row.
/// `home::collapsed_text_width` subtracts this from `sidebar_w` to
/// size the text column in expanded mode.
pub const SIDEBAR_TEXT_CHROME: f32 = SIDEBAR_COLLAPSED + SIDEBAR_TEXT_SPACER;

/// Minimum *expanded* width — thumb + roughly 8 chars of title text
/// before the splitter snaps back into collapsed mode.
pub const SIDEBAR_MIN: f32 = SIDEBAR_COLLAPSED + SP_40;

pub const SIDEBAR_MAX: f32 = SP_80 + SP_44; // 320 + 176 = 496

/// Cross-over between expanded and collapsed reactive layouts. Sits
/// halfway between the two so the text/header/chip binds flip at the
/// same moment the splitter snap fires.
pub const SIDEBAR_COLLAPSE_THRESHOLD: f32 = (SIDEBAR_COLLAPSED + SIDEBAR_MIN) / 2.0;

/// Gutter between the centre pane and the now-playing pane. Folded into
/// the pane's animated width so a collapse shrinks to a true 0. (The
/// pane itself has no width token — its width follows its measured
/// height at the Canvas 9:16 aspect and is not user-resizable.)
pub const NOW_PLAYING_GUTTER: f32 = SP_2;

/// Search input chrome (top bar).
pub const SEARCH_W: f32 = 440.0;
pub const SEARCH_H: f32 = SP_10;

/// Top-bar pill button (Menu/Chevron) dimensions.
pub const TOPBAR_BTN: f32 = SP_9;

// ============================================================================
// Palette
//
// Flat list — no light/dark mode, no theme provider, no semantic alias
// layer. Just the colours the UI actually paints.
// ============================================================================

pub const BG: [f32; 4] = [0.06, 0.06, 0.07, 1.0];
pub const PANEL: [f32; 4] = [0.09, 0.09, 0.11, 1.0];
pub const PANEL_HI: [f32; 4] = [0.13, 0.13, 0.16, 1.0];
pub const BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.06];

pub const TEXT: [f32; 4] = [0.95, 0.95, 0.96, 1.0];
pub const TEXT_DIM: [f32; 4] = [0.65, 0.65, 0.70, 1.0];

// Opal-iridescent brand accent (#A78BFF) — the violet that bridges the
// logo's cyan→lavender→pink. Used as the fallback when no album-art accent
// has been extracted (the dynamic cover accent overrides it for most chrome).
pub const ACCENT: [f32; 4] = [0.655, 0.545, 1.0, 1.0];
pub const ACCENT_HOVER: [f32; 4] = [0.737, 0.651, 1.0, 1.0];

pub const CLOSE_HOVER: [f32; 4] = [0.85, 0.20, 0.20, 1.0];
pub const BTN_HOVER: [f32; 4] = [1.0, 1.0, 1.0, 0.08];

/// Generic dim placeholder behind images while they load.
pub const PLACEHOLDER: [f32; 4] = [0.22, 0.22, 0.27, 1.0];

/// Modal scrim — dims the app behind an open overlay/popup.
pub const SCRIM: [f32; 4] = [0.0, 0.0, 0.0, 0.55];

/// Hover lift used on cards/tiles — slight whiten.
pub const HOVER_LIFT: [f32; 4] = [1.0, 1.0, 1.0, 0.10];
/// Subtle row hover (sidebar list items).
pub const HOVER_LIFT_SUBTLE: [f32; 4] = [1.0, 1.0, 1.0, 0.04];
