use std::fmt;

#[derive(Debug)]
pub enum AuthError {
    /// The credentials themselves were rejected (revoked token, denied
    /// login) — re-auth is the only fix, retrying is pointless.
    Credentials(String),
    Server(String),
    Parse(String),
    Timeout(String),
    Api(String, Option<u16>),
    Http(reqwest::Error),
    Io(std::io::Error),
    Keyring(keyring_core::Error),
    Serde(serde_json::Error),
    Utf8(std::str::Utf8Error),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Credentials(s) => write!(f, "credentials: {s}"),
            AuthError::Server(s) => write!(f, "server: {s}"),
            AuthError::Parse(s) => write!(f, "parse: {s}"),
            AuthError::Timeout(s) => write!(f, "timeout: {s}"),
            AuthError::Api(body, code) => {
                let code = code
                    .map(|c| format!("HTTP {c}"))
                    .unwrap_or_else(|| "api".into());
                match api_error_summary(body) {
                    Some(summary) => write!(f, "{code}: {summary}"),
                    None => write!(f, "{code}: {body}"),
                }
            }
            AuthError::Http(e) => write!(f, "http: {e}"),
            AuthError::Io(e) => write!(f, "io: {e}"),
            AuthError::Keyring(e) => write!(f, "keyring: {e}"),
            AuthError::Serde(e) => write!(f, "serde: {e}"),
            AuthError::Utf8(e) => write!(f, "utf8: {e}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The human part of a JSON error body, so logs and toasts read
/// "HTTP 500: Server error." rather than a pretty-printed blob. Covers both
/// shapes Spotify returns — Web API `{"error":{"status","message","reason"}}`
/// and OAuth `{"error":"…","error_description":"…"}`. `None` for anything
/// else (the raw body is shown). Display-only: matching code (e.g.
/// `NO_ACTIVE_DEVICE`) keeps reading the raw body.
fn api_error_summary(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = v.get("error")?;
    let text = |k: &str, o: &serde_json::Value| o.get(k)?.as_str().map(str::to_owned);
    if err.is_object() {
        let message = text("message", err)?;
        Some(match text("reason", err) {
            Some(reason) => format!("{message} ({reason})"),
            None => message,
        })
    } else {
        let code = err.as_str()?;
        Some(match text("error_description", &v) {
            Some(desc) => format!("{desc} ({code})"),
            None => code.to_owned(),
        })
    }
}

impl From<reqwest::Error> for AuthError {
    fn from(e: reqwest::Error) -> Self {
        AuthError::Http(e)
    }
}
impl From<std::io::Error> for AuthError {
    fn from(e: std::io::Error) -> Self {
        AuthError::Io(e)
    }
}
impl From<keyring_core::Error> for AuthError {
    fn from(e: keyring_core::Error) -> Self {
        AuthError::Keyring(e)
    }
}
impl From<serde_json::Error> for AuthError {
    fn from(e: serde_json::Error) -> Self {
        AuthError::Serde(e)
    }
}
impl From<std::str::Utf8Error> for AuthError {
    fn from(e: std::str::Utf8Error) -> Self {
        AuthError::Utf8(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_display_the_human_message() {
        let web = AuthError::Api(
            "{
  \"error\" : {
    \"status\" : 404,
    \"message\" : \"Player command failed: No active device found\",
    \"reason\" : \"NO_ACTIVE_DEVICE\"
  }
}"
            .into(),
            Some(404),
        );
        assert_eq!(
            web.to_string(),
            "HTTP 404: Player command failed: No active device found (NO_ACTIVE_DEVICE)"
        );
        let oauth = AuthError::Api(
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#.into(),
            Some(400),
        );
        assert_eq!(
            oauth.to_string(),
            "HTTP 400: Refresh token revoked (invalid_grant)"
        );
        let raw = AuthError::Api("Bad gateway".into(), Some(502));
        assert_eq!(raw.to_string(), "HTTP 502: Bad gateway");
    }
}
