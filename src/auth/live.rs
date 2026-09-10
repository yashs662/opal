//! The live Web API credentials, read at **send time** by every request.
//!
//! Tokens used to be copied into each worker command when the UI issued
//! it, so a request carried whatever the UI knew at that moment. Two ways
//! that went stale: the proactive refresh ran off the frame loop (parked
//! or asleep ⇒ no ticks), and its deadline was a monotonic `Instant`,
//! which on macOS stops during sleep — the token expired in wall-clock
//! terms while the deadline said "not yet". Every request after a long
//! sleep then 401'd until something woke the refresh.
//!
//! Now there is one process-wide copy. [`bearer`] hands out the access
//! token, refreshing first when it is inside the expiry margin **by wall
//! clock**; a request that still gets a 401 forces one refresh and
//! retries (see `api::send`). Refreshes are single-flight, persisted, and
//! announced to the UI through the hook the worker installs, so the auth
//! slice stays in step without owning the timing.

use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::auth::oauth::{self, SpotifyAuthResponse};
use crate::auth::token_manager::{self, StoredTokens};
use crate::errors::AuthError;

/// Refresh once the token has less than this left. Wide enough that a
/// request sent right after the check still lands on a valid token.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

struct Live {
    tokens: StoredTokens,
    client_id: String,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
/// Serialises refreshes so concurrent requests hitting the margin (or a
/// burst of 401s) produce one token-endpoint call, not one each.
static REFRESH_GATE: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
type RefreshHook = Box<dyn Fn(SpotifyAuthResponse) + Send + Sync>;
static ON_REFRESH: OnceLock<RefreshHook> = OnceLock::new();

/// Register the one listener told about every refresh this module
/// performs (the worker forwards it to the UI as `TokensRefreshed`).
pub fn on_refresh(f: impl Fn(SpotifyAuthResponse) + Send + Sync + 'static) {
    let _ = ON_REFRESH.set(Box::new(f));
}

/// Make `auth` the live credentials. Called wherever the UI learns of a
/// token: login, startup load, and the refreshes announced by this module
/// (harmless re-install of the same pair).
pub fn install(auth: &SpotifyAuthResponse, client_id: String) {
    let mut g = LIVE.lock().unwrap_or_else(|p| p.into_inner());
    let prev = g.as_ref().map(|l| l.tokens.clone());
    let tokens = StoredTokens::from_refresh(auth.clone(), prev.as_ref());
    *g = Some(Live { tokens, client_id });
}

/// Sign-out: nothing is live any more.
pub fn clear() {
    *LIVE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

pub fn is_installed() -> bool {
    LIVE.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// A token still good for at least the margin, or `None` (none installed
/// or inside the margin).
fn fresh_access() -> Option<String> {
    let g = LIVE.lock().unwrap_or_else(|p| p.into_inner());
    let live = g.as_ref()?;
    (live.tokens.expires_at > now_secs() + EXPIRY_MARGIN.as_secs())
        .then(|| live.tokens.access_token.clone())
}

/// The access token to send now. `None` when nothing is installed (the
/// caller falls back to whatever token it was handed — tests, startup
/// races); otherwise a token that is valid for at least the margin,
/// refreshed here if it wasn't.
pub async fn bearer() -> Option<Result<String, AuthError>> {
    if !is_installed() {
        return None;
    }
    if let Some(t) = fresh_access() {
        return Some(Ok(t));
    }
    Some(refresh().await.map(|a| a.access_token))
}

/// Refresh the live token now — proactively, or after a 401 proved the
/// current one dead. Single-flight: a caller that queued behind another
/// refresh gets that result instead of issuing a second one. Persists the
/// rotated pair and notifies the hook.
pub async fn refresh() -> Result<SpotifyAuthResponse, AuthError> {
    let _gate = REFRESH_GATE.lock().await;
    // Someone ahead of us in the gate may already have done the work.
    if let Some(t) = fresh_access() {
        return Ok(current().ok_or_else(signed_out)?.with_access(t));
    }
    let (refresh_token, client_id) = {
        let g = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        let live = g.as_ref().ok_or_else(signed_out)?;
        (live.tokens.refresh_token.clone(), live.client_id.clone())
    };
    let auth = oauth::refresh_token(&refresh_token, &client_id).await?;
    // The persisted pair carries the refresh token's true issue date (the
    // 180-day cap counts from there); the in-memory copy may not.
    let prev = token_manager::load_tokens().ok();
    let stored = StoredTokens::from_refresh(auth.clone(), prev.as_ref());
    if let Err(e) = token_manager::save_tokens(&stored) {
        log::warn!("persisting refreshed tokens: {e}");
    }
    {
        let mut g = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(live) = g.as_mut() {
            live.tokens = stored;
        }
    }
    log::info!("access token refreshed");
    if let Some(hook) = ON_REFRESH.get() {
        hook(auth.clone());
    }
    Ok(auth)
}

/// `true` for the token endpoint's `invalid_grant`: the refresh token
/// itself is dead and only a new login helps — everything else is a
/// transient failure worth retrying.
pub fn is_permanent(e: &AuthError) -> bool {
    matches!(e, AuthError::Api(body, _) if body.contains("invalid_grant"))
}

fn current() -> Option<SpotifyAuthResponse> {
    LIVE.lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|l| l.tokens.to_auth_response())
}

fn signed_out() -> AuthError {
    AuthError::Credentials("no live Web API credentials".to_string())
}

trait WithAccess {
    fn with_access(self, access: String) -> Self;
}

impl WithAccess for SpotifyAuthResponse {
    fn with_access(mut self, access: String) -> Self {
        self.access_token = access;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(expires_in: u64) -> SpotifyAuthResponse {
        SpotifyAuthResponse {
            access_token: format!("access-{expires_in}"),
            token_type: "Bearer".into(),
            expires_in,
            refresh_token: "refresh".into(),
            scope: String::new(),
        }
    }

    /// One test for the whole lifecycle — the store is process-global, so
    /// separate tests would race on it.
    #[tokio::test]
    async fn bearer_follows_the_installed_token_and_its_margin() {
        clear();
        assert!(
            bearer().await.is_none(),
            "nothing installed → caller's own token"
        );

        install(&auth(3600), "client".into());
        assert_eq!(bearer().await.unwrap().unwrap(), "access-3600");

        // Inside the margin: not handed out as-is (a refresh would run).
        install(&auth(EXPIRY_MARGIN.as_secs() / 2), "client".into());
        assert!(fresh_access().is_none());

        clear();
        assert!(!is_installed());
        assert!(bearer().await.is_none());
    }

    #[test]
    fn only_invalid_grant_is_permanent() {
        assert!(is_permanent(&AuthError::Api(
            r#"{"error":"invalid_grant"}"#.into(),
            Some(400)
        )));
        assert!(!is_permanent(&AuthError::Api(
            "server error".into(),
            Some(503)
        )));
        assert!(!is_permanent(&AuthError::Server("dns".into())));
    }
}
