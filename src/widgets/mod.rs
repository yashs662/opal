//! Cross-view reusable building blocks — primitives and small widgets
//! shared by every view (so they live here, not inside any one view).
//!
//! - [`component`] — the `Component` trait every view region implements.
//! - [`tokens`] — design tokens (spacing/radius/colours).
//! - [`icon`] / [`splitter`] / [`chrome`] — input/layout primitives.
//! - [`chip`] / [`thumb`] / [`crossfade`] / [`color`] — shared widgets +
//!   colour helpers.

pub mod artist_links;
pub mod button;
pub mod chip;
pub mod chrome;
pub mod color;
pub mod component;
pub mod crossfade;
pub mod icon;
pub mod splitter;
pub mod thumb;
pub mod tokens;
