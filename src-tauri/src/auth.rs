//! "Sign in with Claude" — OAuth 2.0 PKCE login so anyone can connect their own
//! Anthropic account, without needing Claude Code installed/signed-in.
//!
//! Uses the same public OAuth client as Claude Code itself (the flow `claude
//! auth login` performs): browser opens `claude.ai/oauth/authorize`, the user
//! approves and gets a `code#state` string to paste back, and we exchange it at
//! the token endpoint with the PKCE verifier. Tokens are stored in the app
//! config dir (`auth.json`, 0600) and refreshed via `refresh_token` when they
//! expire. The Claude Code Keychain/credentials detection in `usage.rs` remains
//! the zero-config path; this store takes precedence when present.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPE: &str = "org:create_api_key user:profile user:inference";
/// Refresh this many seconds before the access token actually expires.
const EXPIRY_SLACK: u64 = 60;

// App config dir, provided once by lib.rs at setup (path APIs need the app handle).
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_config_dir(dir: PathBuf) {
    let _ = CONFIG_DIR.set(dir);
}

fn auth_file() -> Option<PathBuf> {
    CONFIG_DIR.get().map(|d| d.join("auth.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAuth {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_auth() -> Option<StoredAuth> {
    serde_json::from_str(&std::fs::read_to_string(auth_file()?).ok()?).ok()
}

fn save_auth(auth: &StoredAuth) -> Result<(), String> {
    let path = auth_file().ok_or("config dir not initialized")?;
    let json = serde_json::to_string(auth).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// PKCE flow
// ---------------------------------------------------------------------------

struct Pending {
    verifier: String,
    state: String,
}

fn pending() -> &'static Mutex<Option<Pending>> {
    static P: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(None))
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Pure: strip the `#state` / query suffix off the code the user pastes.
pub fn parse_pasted_code(input: &str) -> String {
    let s = input.trim();
    s.split('#').next().unwrap_or(s).split('&').next().unwrap_or(s).trim().to_string()
}

/// Pure: token-endpoint response → StoredAuth. `fallback_refresh` keeps the old
/// refresh token when the server omits a new one (common on refresh grants).
pub fn parse_token_response(
    json: &str,
    now: u64,
    fallback_refresh: Option<&str>,
) -> Result<StoredAuth, String> {
    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }
    let r: TokenResponse =
        serde_json::from_str(json).map_err(|e| format!("bad token response: {e}"))?;
    let refresh = r
        .refresh_token
        .or_else(|| fallback_refresh.map(str::to_string))
        .unwrap_or_default();
    Ok(StoredAuth {
        access_token: r.access_token,
        refresh_token: refresh,
        expires_at: now + r.expires_in.unwrap_or(3600),
    })
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

/// Pure: authorize URL for a given PKCE verifier + state.
fn build_authorize_url(verifier: &str, state: &str) -> String {
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={CLIENT_ID}&response_type=code\
         &redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode(SCOPE),
    )
}

/// Start the login: generate PKCE verifier + state, open the browser, return
/// the URL (shown in the UI as a fallback link).
pub fn begin_login() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let verifier = b64url(&buf);
    let mut sbuf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut sbuf);
    let state: String = sbuf.iter().map(|b| format!("{b:02x}")).collect();

    let url = build_authorize_url(&verifier, &state);
    *pending().lock().unwrap() = Some(Pending { verifier, state });
    open_browser(&url);
    url
}

/// Finish the login: exchange the pasted code for tokens and persist them.
pub fn complete_login(code_input: &str) -> Result<(), String> {
    let (verifier, state) = {
        let guard = pending().lock().unwrap();
        let p = guard.as_ref().ok_or("login not started — click Sign in first")?;
        (p.verifier.clone(), p.state.clone())
    };
    let code = parse_pasted_code(code_input);
    if code.is_empty() {
        return Err("empty code".to_string());
    }
    let resp = ureq::post(TOKEN_URL)
        .timeout(std::time::Duration::from_secs(15))
        .send_json(serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
            "state": state,
        }))
        .map_err(|e| match e {
            ureq::Error::Status(s, r) => format!(
                "token exchange failed (HTTP {s}): {}",
                r.into_string().unwrap_or_default().chars().take(200).collect::<String>()
            ),
            other => format!("token exchange failed: {other}"),
        })?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    let auth = parse_token_response(&body, now_unix(), None)?;
    save_auth(&auth)?;
    pending().lock().unwrap().take();
    Ok(())
}

fn refresh(auth: &StoredAuth) -> Result<StoredAuth, String> {
    if auth.refresh_token.is_empty() {
        return Err("no refresh token".to_string());
    }
    let resp = ureq::post(TOKEN_URL)
        .timeout(std::time::Duration::from_secs(15))
        .send_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": auth.refresh_token,
        }))
        .map_err(|e| format!("token refresh failed: {e}"))?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    parse_token_response(&body, now_unix(), Some(&auth.refresh_token))
}

/// A valid access token from the widget's own login, refreshing if needed.
/// `None` when the user never signed in here (callers fall back to the Claude
/// Code Keychain/credentials detection).
pub fn current_access_token() -> Option<String> {
    let auth = load_auth()?;
    if auth.expires_at > now_unix() + EXPIRY_SLACK {
        return Some(auth.access_token);
    }
    match refresh(&auth) {
        Ok(new_auth) => {
            let _ = save_auth(&new_auth);
            Some(new_auth.access_token)
        }
        Err(_) => None, // expired + unrefreshable → treat as signed out
    }
}

pub fn signed_in() -> bool {
    load_auth().is_some()
}

pub fn sign_out() {
    if let Some(path) = auth_file() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_code_strips_state_fragment() {
        assert_eq!(parse_pasted_code("abc123#deadbeef"), "abc123");
        assert_eq!(parse_pasted_code("  abc123  "), "abc123");
        assert_eq!(parse_pasted_code("abc123&foo=bar"), "abc123");
        assert_eq!(parse_pasted_code("abc123"), "abc123");
        assert_eq!(parse_pasted_code(""), "");
    }

    #[test]
    fn token_response_parses_and_computes_expiry() {
        let a = parse_token_response(
            r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":7200}"#,
            1000,
            None,
        )
        .unwrap();
        assert_eq!(a.access_token, "at-1");
        assert_eq!(a.refresh_token, "rt-1");
        assert_eq!(a.expires_at, 8200);
    }

    #[test]
    fn token_response_keeps_old_refresh_when_omitted() {
        let a = parse_token_response(
            r#"{"access_token":"at-2","expires_in":3600}"#,
            0,
            Some("rt-old"),
        )
        .unwrap();
        assert_eq!(a.refresh_token, "rt-old");
        assert_eq!(a.expires_at, 3600);
    }

    #[test]
    fn token_response_bad_json_is_err() {
        assert!(parse_token_response("nope", 0, None).is_err());
    }

    #[test]
    fn authorize_url_has_required_params() {
        let url = build_authorize_url("test-verifier", "test-state");
        for needle in [
            "client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "response_type=code",
            "code_challenge_method=S256",
            "code_challenge=",
            "state=test-state",
            "scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference",
        ] {
            assert!(url.contains(needle), "missing {needle} in {url}");
        }
    }
}
