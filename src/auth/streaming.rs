//! The librespot session's own OAuth grant.
//!
//! Opal runs *two* independent grants. The Web API one (see [`super::oauth`])
//! is minted under the user's own client id, which is what Spotify's
//! developer terms expect for library/playback REST calls. The session,
//! though, talks to first-party services — `clienttoken` refuses to issue a
//! client token for an app id it doesn't recognise, and `login5` refuses a
//! stored credential minted under an app id other than the one in the
//! request. A user's dev-mode app therefore cannot back a librespot session
//! at all, so this module mints a second token under
//! [`STREAMING_CLIENT_ID`], stored in its own keyring slot.
//!
//! Interactive consent happens at most once: the refresh token is persisted
//! and every later launch refreshes silently.

use keyring_core::Entry;
use librespot_oauth::OAuthClientBuilder;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

use crate::constants::{
    CREDENTIAL_SERVICE_NAME, STREAMING_CLIENT_ID, STREAMING_CREDENTIAL_USER_NAME,
    STREAMING_REDIRECT_URI, STREAMING_SCOPES,
};
use crate::errors::AuthError;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamingTokens {
    access_token: String,
    refresh_token: String,
    /// Unix seconds at which `access_token` stops being valid.
    expires_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

impl StreamingTokens {
    /// A minute of slack so a token can't expire in flight.
    fn is_expired(&self) -> bool {
        self.expires_at < now_secs() + 60
    }
}

fn entry() -> Result<Entry, AuthError> {
    Ok(Entry::new(
        CREDENTIAL_SERVICE_NAME,
        STREAMING_CREDENTIAL_USER_NAME,
    )?)
}

fn load() -> Result<StreamingTokens, AuthError> {
    let bytes = entry()?.get_secret()?;
    Ok(serde_json::from_str(std::str::from_utf8(&bytes)?)?)
}

fn save(t: &StreamingTokens) -> Result<(), AuthError> {
    entry()?.set_secret(serde_json::to_string(t)?.as_bytes())?;
    Ok(())
}

/// Drop the stored streaming grant — used on sign-out so the next user
/// doesn't inherit this one's session.
pub fn delete() -> Result<(), AuthError> {
    entry()?.delete_credential()?;
    Ok(())
}

/// Convert a fresh [`librespot_oauth::OAuthToken`] into our persisted shape.
/// Its `expires_at` is an `Instant` (process-local), so re-derive the wall
/// clock from the remaining lifetime.
fn to_stored(t: librespot_oauth::OAuthToken) -> StreamingTokens {
    let ttl = t
        .expires_at
        .saturating_duration_since(std::time::Instant::now())
        .as_secs();
    StreamingTokens {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        expires_at: now_secs() + ttl,
    }
}

fn client() -> Result<librespot_oauth::OAuthClient, AuthError> {
    OAuthClientBuilder::new(
        STREAMING_CLIENT_ID,
        STREAMING_REDIRECT_URI,
        STREAMING_SCOPES.to_vec(),
    )
    .open_in_browser()
    .build()
    .map_err(|e| AuthError::Server(format!("streaming oauth client: {e}")))
}

/// A valid access token for the librespot session, minting one if needed.
///
/// Cached → refreshed → interactive, in that order. The interactive path
/// opens a browser and blocks on a loopback listener, so it runs on a
/// blocking thread rather than stalling the worker's runtime.
pub async fn access_token() -> Result<String, AuthError> {
    if let Ok(t) = load()
        && !t.is_expired()
    {
        return Ok(t.access_token);
    }

    let stored_refresh = load().ok().map(|t| t.refresh_token);
    if let Some(rt) = stored_refresh {
        let c = client()?;
        match c.refresh_token_async(&rt).await {
            Ok(tok) => {
                let stored = to_stored(tok);
                let _ = save(&stored);
                info!("streaming session token refreshed");
                return Ok(stored.access_token);
            }
            // A revoked/expired grant can't be refreshed — fall through to
            // a fresh consent rather than leaving the session dead.
            Err(e) => warn!("streaming token refresh failed ({e}) — re-authorising"),
        }
    }

    info!("requesting streaming session authorisation");
    let tok = tokio::task::spawn_blocking(move || {
        client()?
            .get_access_token()
            .map_err(|e| AuthError::Server(format!("streaming oauth: {e}")))
    })
    .await
    .map_err(|e| AuthError::Server(format!("streaming oauth task: {e}")))??;

    let stored = to_stored(tok);
    let _ = save(&stored);
    Ok(stored.access_token)
}
