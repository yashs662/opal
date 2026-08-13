pub const CREDENTIAL_SERVICE_NAME: &str = "Opal";
pub const CREDENTIAL_USER_NAME: &str = "Opal_user";
/// Keyring slot for the streaming-session tokens — a *second*, independent
/// OAuth grant (see [`STREAMING_CLIENT_ID`]) that must not collide with the
/// Web API tokens under [`CREDENTIAL_USER_NAME`].
pub const STREAMING_CREDENTIAL_USER_NAME: &str = "Opal_streaming";

/// Spotify's own "keymaster" app id, as hardcoded by every librespot-based
/// client. The librespot session needs it: after the access-point handshake,
/// `clienttoken` and `login5` are first-party services that reject an app id
/// they don't recognise (400 / INVALID_CREDENTIALS), and login5 additionally
/// refuses a stored credential minted under a *different* app. So the
/// streaming session runs its own OAuth grant under this id, while every
/// Web API call keeps using the user's own client id.
pub const STREAMING_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
/// Loopback callback for the streaming grant. Distinct port from
/// [`SPOTIFY_REDIRECT_URI`] so both listeners can coexist; the `/login`
/// path is what this app id has registered.
pub const STREAMING_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
/// Scopes for the streaming grant — librespot's own list, i.e. what the
/// desktop client asks for. Only the session uses this token.
pub const STREAMING_SCOPES: &[&str] = &[
    "app-remote-control",
    "playlist-modify",
    "playlist-modify-private",
    "playlist-modify-public",
    "playlist-read",
    "playlist-read-collaborative",
    "playlist-read-private",
    "streaming",
    "ugc-image-upload",
    "user-follow-modify",
    "user-follow-read",
    "user-library-modify",
    "user-library-read",
    "user-modify",
    "user-modify-playback-state",
    "user-modify-private",
    "user-personalized",
    "user-read-birthdate",
    "user-read-currently-playing",
    "user-read-email",
    "user-read-play-history",
    "user-read-playback-position",
    "user-read-playback-state",
    "user-read-private",
    "user-read-recently-played",
    "user-top-read",
];
pub const SPOTIFY_REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
pub const SPOTIFY_ACCESS_SCOPES: &str = "streaming,user-read-email,user-read-private,playlist-read-private,playlist-read-collaborative,playlist-modify-public,playlist-modify-private,user-follow-modify,user-follow-read,user-library-read,user-library-modify,user-top-read,user-read-recently-played,user-read-playback-state,user-read-currently-playing,user-modify-playback-state";

pub const LOGIN_OK_HTML: &str = r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>Opal</title><style>body{display:flex;flex-direction:column;justify-content:center;align-items:center;height:100vh;text-align:center;font-family:Arial,sans-serif;background:#121212;color:#fff}.m{font-size:20px}.c{margin-top:10px;font-size:14px;color:#aaa}</style><script>let t=5;onload=()=>{let e=document.getElementById('c');let i=setInterval(()=>{t--;e.textContent=`Closing in ${t}s...`;if(t<=0){clearInterval(i);window.close();}},1000);}</script></head><body><div class="m">Authentication successful!</div><div class="c" id="c">Closing in 5s...</div></body></html>"#;

pub const LOGIN_ERR_HTML: &str = r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>Opal</title><style>body{display:flex;justify-content:center;align-items:center;height:100vh;font-family:Arial;background:#121212;color:#fff}</style></head><body><div>Login error. Try again.</div></body></html>"#;
