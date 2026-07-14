use std::fmt;

#[derive(Debug)]
pub enum AuthError {
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
            AuthError::Server(s) => write!(f, "server: {s}"),
            AuthError::Parse(s) => write!(f, "parse: {s}"),
            AuthError::Timeout(s) => write!(f, "timeout: {s}"),
            AuthError::Api(s, c) => write!(f, "api ({c:?}): {s}"),
            AuthError::Http(e) => write!(f, "http: {e}"),
            AuthError::Io(e) => write!(f, "io: {e}"),
            AuthError::Keyring(e) => write!(f, "keyring: {e}"),
            AuthError::Serde(e) => write!(f, "serde: {e}"),
            AuthError::Utf8(e) => write!(f, "utf8: {e}"),
        }
    }
}

impl std::error::Error for AuthError {}

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
