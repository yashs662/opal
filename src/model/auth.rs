//! Authentication slice — the live Spotify OAuth session.
//!
//! Thin holder around the current [`SpotifyAuthResponse`]. Most call
//! sites only want the access token for a worker command, so [`token`]
//! collapses the former repeated
//! `auth.borrow().as_ref().map(|a| a.access_token.clone())` dance into
//! one accessor.
//!
//! [`token`]: AuthModel::token

use std::time::{Duration, Instant};

use crate::auth::oauth::SpotifyAuthResponse;

/// Refresh this long before the token actually expires — wide enough
/// that every in-flight request still carries a valid token, narrow
/// enough that we don't refresh more than ~once an hour.
const REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// Back-off between refresh attempts after a failure (network blip,
/// Spotify 5xx) so the frame tick doesn't hammer the token endpoint.
const REFRESH_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct AuthModel {
    current: Option<SpotifyAuthResponse>,
    /// When the live access token should be proactively refreshed
    /// (`expires_in` minus [`REFRESH_MARGIN`]). `None` = signed out.
    refresh_at: Option<Instant>,
    /// A refresh request is in flight — gate so the per-frame due-check
    /// dispatches exactly one.
    refresh_inflight: bool,
}

impl AuthModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone the live access token, or `None` when signed out. Read at
    /// fire time so it survives a token refresh.
    pub fn token(&self) -> Option<String> {
        self.current.as_ref().map(|a| a.access_token.clone())
    }

    pub fn set(&mut self, auth: SpotifyAuthResponse) {
        // Never sooner than a minute out, so a token that arrives
        // nearly-expired can't put the due-check into a tight loop.
        let lead = Duration::from_secs(auth.expires_in.max(360)) - REFRESH_MARGIN;
        self.refresh_at = Some(Instant::now() + lead);
        self.refresh_inflight = false;
        self.current = Some(auth);
    }

    pub fn clear(&mut self) {
        self.refresh_at = None;
        self.refresh_inflight = false;
        self.current = None;
    }

    /// If the access token is due for a proactive refresh, return the
    /// refresh token (once — flips the in-flight gate). Called every
    /// frame tick; cheap (two field reads on the cold path).
    pub fn refresh_due(&mut self, now: Instant) -> Option<String> {
        if self.refresh_inflight || self.refresh_at.is_none_or(|t| now < t) {
            return None;
        }
        let rt = self.current.as_ref()?.refresh_token.clone();
        self.refresh_inflight = true;
        Some(rt)
    }

    /// A refresh attempt failed — back off and try again shortly.
    pub fn refresh_failed(&mut self) {
        self.refresh_at = Some(Instant::now() + REFRESH_RETRY);
        self.refresh_inflight = false;
    }

    /// Sign out: delete the persisted token from the OS store and drop the
    /// in-memory session. (The caller handles the view switch / modal
    /// reset — those are shell concerns.)
    pub fn sign_out(&mut self) {
        if let Err(e) = crate::auth::token_manager::delete_tokens() {
            log::warn!("sign-out: failed to clear stored token: {e}");
        }
        self.clear();
    }
}
