//! App-level control plumbing — the per-frame/per-event context, the frame
//! loop tick, and the worker-response reducer. This is the "shell" logic
//! that drives the models + views but isn't itself a view; it lives here
//! so `main` stays pure scaffolding.

pub mod cx;
pub mod frame;
pub mod msg;
pub mod reducer;
pub mod state;
pub mod update;

pub use state::AppState;
