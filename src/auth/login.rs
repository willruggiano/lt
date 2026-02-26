use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::io::{Read, Write as _};
use std::net::TcpListener;

use crate::config::{self, AuthToken};

use tracing::info;

const CALLBACK_PORT: u16 = 7342;
const AUTH_URL: &str = "https://linear.app/oauth/authorize";
const TOKEN_URL: &str = "https://api.linear.app/oauth/token";

pub fn run() -> Result<()> {
    let (client_id, client_secret) = resolve_credentials()?;

    let (code_verifier, code_challenge) = generate_pkce();
    let state = random_base64(16);
    let redirect_uri = format!("http://localhost:{}/callback", CALLBACK_PORT);

    let auth_url = build_auth_url(&client_id, &redirect_uri, &state, &code_challenge);

    info!("Opening Linear authorization page in your browser...");
    info!("If the browser does not open, visit: {}", auth_url);

    // Best-effort: ignore errors from open (headless environments, etc.)
    let _ = open::that(&auth_url);

    let code = listen_for_callback(CALLBACK_PORT, &state).context("waiting for OAuth callback")?;

    info!("Authorization received. Exchanging for token...");

    let token = exchange_code(
        &client_id,
        &client_secret,
        &code,
        &redirect_uri,
        &code_verifier,
    )
    .context("exchanging authorization code for token")?;

    config::save_token(&token)?;
    println!("Logged in to Linear.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Credential resolution
// ---------------------------------------------------------------------------

fn resolve_credentials() -> Result<(String, String)> {
    // 1. Environment variables take precedence.
    if let (Ok(id), Ok(secret)) = (
        std::env::var("LINEAR_CLIENT_ID"),
        std::env::var("LINEAR_CLIENT_SECRET"),
    ) {
        return Ok((id, secret));
    }

    // 2. Stored config file.
    let cfg = config::load_config()?;
    if let (Some(id), Some(secret)) = (cfg.client_id, cfg.client_secret) {
        return Ok((id, secret));
    }

    // 3. Interactive prompt.
    info!("No Linear OAuth credentials found.");
    info!(
        "Register an application at: https://linear.app/settings/api/applications"
    );
    info!(
        "Set the redirect URI to: http://localhost:{}/callback",
        CALLBACK_PORT
    );

    let client_id = prompt("Client ID: ")?;
    let client_secret = prompt("Client Secret: ")?;

    if client_id.is_empty() || client_secret.is_empty() {
        return Err(anyhow!("client ID and client secret are required"));
    }

    Ok((client_id, client_secret))
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{}", label);
    std::io::stderr().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

// ---------------------------------------------------------------------------
// PKCE (RFC 7636) and state generation
// ---------------------------------------------------------------------------

fn generate_pkce() -> (String, String) {
    let code_verifier = random_base64(32);

    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let hash = hasher.finalize();
    let code_challenge = URL_SAFE_NO_PAD.encode(hash);

    (code_verifier, code_challenge)
}

/// Generate `n` random bytes, base64url-encoded (no padding).
fn random_base64(n: usize) -> String {
    use rand::RngCore as _;
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

// ---------------------------------------------------------------------------
// Authorization URL
// ---------------------------------------------------------------------------

fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let mut u = url::Url::parse(AUTH_URL).expect("AUTH_URL is valid");
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state)
        .append_pair("scope", "read,write")
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    u.to_string()
}

// ---------------------------------------------------------------------------
// Local HTTP callback server
// ---------------------------------------------------------------------------

/// Bind a TCP listener on `port` and block until we receive a valid OAuth
/// callback.  Returns the `code` query parameter.
fn listen_for_callback(port: u16, expected_state: &str) -> Result<String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .with_context(|| format!("binding callback listener on port {}", port))?;

    info!("Listening for callback on http://localhost:{}/ ...", port);

    loop {
        let (mut stream, _peer) = listener.accept().context("accepting connection")?;

        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).context("reading HTTP request")?;
        let raw = std::str::from_utf8(&buf[..n]).unwrap_or("");

        // The request line is: GET /path?query HTTP/1.1
        let request_line = raw.lines().next().unwrap_or("");
        let path_and_query = request_line.split_whitespace().nth(1).unwrap_or("/");

        // Only handle the callback path; ignore /favicon.ico etc.
        let (path, query) = match path_and_query.split_once('?') {
            Some((p, q)) => (p, q),
            None => (path_and_query, ""),
        };

        if path != "/callback" {
            let _ = http_reply(&mut stream, 404, "Not found");
            continue;
        }

        // Parse query parameters.
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(query.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();

        // CSRF check.
        if params.get("state").map(String::as_str) != Some(expected_state) {
            let _ = http_reply(&mut stream, 400, "State mismatch");
            return Err(anyhow!("state mismatch in OAuth callback (possible CSRF)"));
        }

        match params.get("code") {
            Some(code) => {
                let _ = http_reply(
                    &mut stream,
                    200,
                    "<html><body><h2>Authorization complete.</h2>\
                     <p>You may close this tab.</p></body></html>",
                );
                return Ok(code.clone());
            }
            None => {
                let error = params
                    .get("error")
                    .map(String::as_str)
                    .unwrap_or("unknown error");
                let _ = http_reply(&mut stream, 400, "Authorization failed");
                return Err(anyhow!("authorization denied: {}", error));
            }
        }
    }
}

fn http_reply(stream: &mut impl std::io::Write, status: u16, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        status,
        reason,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<AuthToken> {
    let params: &[(&str, &str)] = &[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];

    match ureq::post(TOKEN_URL).send_form(params) {
        Ok(resp) => Ok(resp
            .into_json::<AuthToken>()
            .context("parsing token response")?),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(anyhow!("token exchange failed (HTTP {}): {}", code, body))
        }
        Err(e) => Err(anyhow::Error::from(e)),
    }
}
