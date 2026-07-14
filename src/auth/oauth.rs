use crate::constants::{
    LOGIN_ERR_HTML, LOGIN_OK_HTML, SPOTIFY_ACCESS_SCOPES, SPOTIFY_REDIRECT_URI,
};
use crate::errors::AuthError;
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use log::debug;
use rand::{RngExt, distr::Alphanumeric};
use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

#[derive(Debug, Clone)]
pub struct SpotifyAuthResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    pub scope: String,
}

pub fn get_spotify_auth_url(client_id: &str) -> (String, String) {
    let code_verifier = generate_pkce_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let auth_url = Url::parse_with_params(
        "https://accounts.spotify.com/authorize",
        &[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", SPOTIFY_REDIRECT_URI),
            ("scope", SPOTIFY_ACCESS_SCOPES),
            ("code_challenge_method", "S256"),
            ("code_challenge", &code_challenge),
        ],
    )
    .unwrap()
    .to_string();
    (auth_url, code_verifier)
}

fn generate_pkce_code_verifier() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect()
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier);
    BASE64_URL_SAFE_NO_PAD.encode(h.finalize())
}

async fn write_html(socket: &mut tokio::net::TcpStream, html: &str) -> Result<(), AuthError> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    socket.write_all(resp.as_bytes()).await?;
    socket.flush().await?;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _ = socket.shutdown().await;
    Ok(())
}

pub async fn listen_for_callback(
    code_verifier: String,
    client_id: String,
) -> Result<SpotifyAuthResponse, AuthError> {
    let listener = TcpListener::bind("127.0.0.1:8888")
        .await
        .map_err(|e| AuthError::Server(e.to_string()))?;
    debug!("Listening on http://127.0.0.1:8888...");

    let (mut socket, _) =
        tokio::time::timeout(std::time::Duration::from_secs(120), listener.accept())
            .await
            .map_err(|_| AuthError::Timeout("waiting for callback".into()))?
            .map_err(|e| AuthError::Server(e.to_string()))?;

    let mut buffer = vec![0u8; 4096];
    let mut filled = 0usize;
    loop {
        let n = socket.read(&mut buffer[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
        if buffer[..filled].contains(&b'\n') || filled == buffer.len() {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buffer[..filled]);
    let code = request
        .find("code=")
        .and_then(|s| {
            let after = &request[s + 5..];
            let end = after.find(['&', ' ', '\r', '\n']).unwrap_or(after.len());
            (end > 0).then(|| &after[..end])
        })
        .ok_or_else(|| AuthError::Parse("no code in request".into()))?
        .to_string();

    let client = Client::new();
    let params = [
        ("client_id", client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", SPOTIFY_REDIRECT_URI),
        ("code_verifier", code_verifier.as_str()),
    ];
    let res = client
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        write_html(&mut socket, LOGIN_ERR_HTML).await.ok();
        return Err(AuthError::Api(body, Some(status)));
    }

    let body = res.json::<serde_json::Value>().await?;
    let required = [
        "access_token",
        "token_type",
        "expires_in",
        "refresh_token",
        "scope",
    ];
    if required.iter().any(|k| body.get(k).is_none()) {
        write_html(&mut socket, LOGIN_ERR_HTML).await.ok();
        return Err(AuthError::Parse("bad spotify response".into()));
    }

    write_html(&mut socket, LOGIN_OK_HTML).await?;

    Ok(SpotifyAuthResponse {
        access_token: body["access_token"].as_str().unwrap().to_string(),
        token_type: body["token_type"].as_str().unwrap().to_string(),
        expires_in: body["expires_in"].as_u64().unwrap(),
        refresh_token: body["refresh_token"].as_str().unwrap().to_string(),
        scope: body["scope"].as_str().unwrap().to_string(),
    })
}

pub async fn refresh_token(
    refresh: &str,
    client_id: &str,
) -> Result<SpotifyAuthResponse, AuthError> {
    let client = Client::new();
    let params = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
    ];
    let res = client
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Api(body, Some(status)));
    }
    let body = res.json::<serde_json::Value>().await?;
    let new_refresh = body["refresh_token"]
        .as_str()
        .unwrap_or(refresh)
        .to_string();
    Ok(SpotifyAuthResponse {
        access_token: body["access_token"].as_str().unwrap().to_string(),
        token_type: body["token_type"].as_str().unwrap().to_string(),
        expires_in: body["expires_in"].as_u64().unwrap(),
        refresh_token: new_refresh,
        scope: body["scope"]
            .as_str()
            .unwrap_or(SPOTIFY_ACCESS_SCOPES)
            .to_string(),
    })
}
