use std::io::{Read, Write as _};
use std::net::TcpListener;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use lt_config::{self, AuthToken};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

const CALLBACK_PORT: u16 = 7342;
const AUTH_URL: &str = "https://linear.app/oauth/authorize";
pub(super) const TOKEN_URL: &str = "https://api.linear.app/oauth/token";

/// Non-interactive login: identical to `run()` but errors instead of prompting
/// when OAuth credentials are missing. Safe to call from a background thread
/// while the TUI owns the terminal.
pub fn run_non_interactive() -> Result<()> {
    let (client_id, client_secret) = resolve_credentials_non_interactive()?;
    login_with(&client_id, &client_secret)
}

pub fn run() -> Result<()> {
    let (client_id, client_secret) = resolve_credentials()?;
    login_with(&client_id, &client_secret)
}

/// Run the OAuth flow with the production wiring and persist the token.
fn login_with(client_id: &str, client_secret: &str) -> Result<()> {
    let token = run_with_credentials(&production_flow(), client_id, client_secret)?;
    lt_config::save_token(&token)?;
    info!("Logged in to Linear.");
    Ok(())
}

/// The OAuth side effects `run_with_credentials` needs, injected so tests can
/// drive the flow without a browser, TCP listener, or network.
struct OauthFlow<'a> {
    browser: &'a dyn Browser,
    listener: &'a dyn CallbackListener,
    exchanger: TokenExchanger,
}

/// Production wiring: real browser, TCP callback listener, ureq HTTP.
fn production_flow() -> OauthFlow<'static> {
    OauthFlow {
        browser: &RealBrowser,
        listener: &TcpCallbackListener,
        exchanger: TokenExchanger::Ureq,
    }
}

/// Run the OAuth authorization-code-with-PKCE flow and return the token.
///
/// Persisting the token is the caller's responsibility, so this function stays
/// free of profile/disk state and is drivable end-to-end in tests.
fn run_with_credentials(
    flow: &OauthFlow,
    client_id: &str,
    client_secret: &str,
) -> Result<AuthToken> {
    let (code_verifier, code_challenge) = generate_pkce();
    let state = random_base64(16);
    let redirect_uri = format!("http://localhost:{CALLBACK_PORT}/callback");

    let auth_url = build_auth_url(client_id, &redirect_uri, &state, &code_challenge)?;

    info!("Opening Linear authorization page in your browser...");
    info!("If the browser does not open, visit: {}", auth_url);

    flow.browser.open(&auth_url);

    let code = flow
        .listener
        .wait_for_code(CALLBACK_PORT, &state)
        .context("waiting for OAuth callback")?;

    info!("Authorization received. Exchanging for token...");

    exchange_code(
        &flow.exchanger,
        &TokenExchange {
            client_id,
            client_secret,
            code: &code,
            redirect_uri: &redirect_uri,
            code_verifier: &code_verifier,
        },
    )
    .context("exchanging authorization code for token")
}

// ---------------------------------------------------------------------------
// Browser launch
// ---------------------------------------------------------------------------

/// Opens a URL in the user's browser. Seam so tests can drive login without
/// spawning a browser.
trait Browser {
    fn open(&self, url: &str);
}

struct RealBrowser;

impl Browser for RealBrowser {
    fn open(&self, url: &str) {
        // Best-effort: headless environments etc. still have the URL logged
        // above; a failure to launch a browser is not fatal to the flow.
        if let Err(e) = open::that(url) {
            warn!(error = %e, "failed to open browser for OAuth authorization");
        }
    }
}

// ---------------------------------------------------------------------------
// Credential resolution
// ---------------------------------------------------------------------------

/// Look up credentials from env vars (precedence) then the stored config file.
/// Returns `Ok(None)` when neither source has credentials; propagates errors
/// from loading the config file.
pub(super) fn lookup_stored_credentials() -> Result<Option<(String, String)>> {
    // 1. Environment variables take precedence.
    if let (Ok(id), Ok(secret)) = (
        std::env::var("LINEAR_CLIENT_ID"),
        std::env::var("LINEAR_CLIENT_SECRET"),
    ) {
        return Ok(Some((id, secret)));
    }

    // 2. Stored config file.
    let cfg = lt_config::load_config()?;
    if let (Some(id), Some(secret)) = (cfg.client_id, cfg.client_secret) {
        return Ok(Some((id, secret)));
    }

    Ok(None)
}

/// Resolve credentials without interactive prompting. Checks env vars and
/// config file only; returns an error if neither source has credentials.
fn resolve_credentials_non_interactive() -> Result<(String, String)> {
    lookup_stored_credentials()?.ok_or_else(|| {
        anyhow!(
            "no OAuth credentials configured -- set LINEAR_CLIENT_ID and \
             LINEAR_CLIENT_SECRET env vars or run `lt auth login` from the terminal"
        )
    })
}

fn resolve_credentials() -> Result<(String, String)> {
    // 1. Environment variables, then 2. stored config file.
    if let Some(creds) = lookup_stored_credentials()? {
        return Ok(creds);
    }

    // 3. Interactive prompt.
    info!("No Linear OAuth credentials found.");
    info!("Register an application at: https://linear.app/settings/api/applications");
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
    write!(std::io::stderr(), "{label}")?;
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
    use rand::Rng as _;
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
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
) -> Result<String> {
    let mut u = url::Url::parse(AUTH_URL).context("AUTH_URL is not a valid URL")?;
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state)
        .append_pair("scope", "read,write")
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(u.to_string())
}

// ---------------------------------------------------------------------------
// Local HTTP callback server
// ---------------------------------------------------------------------------

/// The result of parsing one inbound HTTP request to the callback server.
enum CallbackOutcome {
    /// Not the callback path (favicon, etc.) -- reply 404 and keep waiting.
    Ignore,
    /// A valid authorization code arrived.
    Code(String),
    /// The `state` parameter did not match (possible CSRF).
    StateMismatch,
    /// The provider reported an authorization error.
    Denied(String),
}

/// Parse one raw HTTP request into a `CallbackOutcome`. Pure: no IO, so the
/// CSRF / code / error branches are unit-testable with hand-built requests.
fn parse_callback_request(raw: &str, expected_state: &str) -> CallbackOutcome {
    // The request line is: GET /path?query HTTP/1.1
    let request_line = raw.lines().next().unwrap_or("");
    let path_and_query = request_line.split_whitespace().nth(1).unwrap_or("/");

    // Only handle the callback path; ignore /favicon.ico etc.
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_and_query, ""),
    };

    if path != "/callback" {
        return CallbackOutcome::Ignore;
    }

    let params: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

    // CSRF check.
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return CallbackOutcome::StateMismatch;
    }

    if let Some(code) = params.get("code") {
        return CallbackOutcome::Code(code.clone());
    }

    let error = params.get("error").map_or("unknown error", String::as_str);
    CallbackOutcome::Denied(error.to_string())
}

/// Blocks until a valid OAuth callback arrives and yields the `code`.
trait CallbackListener {
    fn wait_for_code(&self, port: u16, expected_state: &str) -> Result<String>;
}

/// Production listener: bind a local TCP socket and serve the callback,
/// dispatching each request through the pure `parse_callback_request`.
struct TcpCallbackListener;

impl CallbackListener for TcpCallbackListener {
    fn wait_for_code(&self, port: u16, expected_state: &str) -> Result<String> {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
            .with_context(|| format!("binding callback listener on port {port}"))?;

        info!("Listening for callback on http://localhost:{}/ ...", port);

        loop {
            let (mut stream, _peer) = listener.accept().context("accepting connection")?;

            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).context("reading HTTP request")?;
            let raw = std::str::from_utf8(&buf[..n]).unwrap_or("");

            match parse_callback_request(raw, expected_state) {
                CallbackOutcome::Ignore => {
                    if let Err(e) = http_reply(&mut stream, 404, "Not found") {
                        warn!(error = %e, "failed to write OAuth callback response");
                    }
                }
                CallbackOutcome::StateMismatch => {
                    if let Err(e) = http_reply(&mut stream, 400, "State mismatch") {
                        warn!(error = %e, "failed to write OAuth callback response");
                    }
                    return Err(anyhow!("state mismatch in OAuth callback (possible CSRF)"));
                }
                CallbackOutcome::Code(code) => {
                    if let Err(e) = http_reply(
                        &mut stream,
                        200,
                        "<html><body><h2>Authorization complete.</h2>\
                         <p>You may close this tab.</p></body></html>",
                    ) {
                        warn!(error = %e, "failed to write OAuth callback response");
                    }
                    return Ok(code);
                }
                CallbackOutcome::Denied(error) => {
                    if let Err(e) = http_reply(&mut stream, 400, "Authorization failed") {
                        warn!(error = %e, "failed to write OAuth callback response");
                    }
                    return Err(anyhow!("authorization denied: {error}"));
                }
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

/// Inputs required to exchange an authorization code for an access token.
struct TokenExchange<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    code: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
}

/// Build the form fields for the token-exchange POST. Pure.
fn build_token_params<'a>(exchange: &TokenExchange<'a>) -> [(&'a str, &'a str); 6] {
    [
        ("grant_type", "authorization_code"),
        ("client_id", exchange.client_id),
        ("client_secret", exchange.client_secret),
        ("code", exchange.code),
        ("redirect_uri", exchange.redirect_uri),
        ("code_verifier", exchange.code_verifier),
    ]
}

/// Validate the HTTP status and deserialize the token body. Pure.
pub(super) fn parse_token_response(status: u16, body: &str) -> Result<AuthToken> {
    if !(200..300).contains(&status) {
        return Err(anyhow!("token exchange failed (HTTP {status}): {body}"));
    }
    serde_json::from_str::<AuthToken>(body).context("parsing token response")
}

/// POSTs a token-grant form and returns the HTTP status and raw body. The
/// set of exchangers is closed -- ureq in production, a scripted fake in
/// tests -- so it is an enum rather than a trait with two impls.
pub(super) enum TokenExchanger {
    /// Send the form via ureq.
    Ureq,
    /// Return a scripted response and record each form it was sent.
    #[cfg(test)]
    Fake {
        status: u16,
        body: String,
        calls: std::cell::RefCell<Vec<Vec<(String, String)>>>,
    },
}

impl TokenExchanger {
    pub(super) fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<(u16, String)> {
        match self {
            Self::Ureq => {
                let result = ureq::post(url)
                    .config()
                    .http_status_as_error(false)
                    .build()
                    .send_form(params.iter().copied());

                match result {
                    Ok(mut resp) => {
                        let status = resp.status().as_u16();
                        let body = resp
                            .body_mut()
                            .read_to_string()
                            .context("reading token exchange response body")?;
                        Ok((status, body))
                    }
                    Err(e) => Err(anyhow::Error::from(e)),
                }
            }
            #[cfg(test)]
            Self::Fake {
                status,
                body,
                calls,
            } => {
                calls.borrow_mut().push(
                    params
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                        .collect(),
                );
                Ok((*status, body.clone()))
            }
        }
    }
}

fn exchange_code(exchanger: &TokenExchanger, exchange: &TokenExchange) -> Result<AuthToken> {
    let params = build_token_params(exchange);
    let (status, body) = exchanger.post_form(TOKEN_URL, &params)?;
    parse_token_response(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let (verifier, challenge) = generate_pkce();
        // Recompute S256(verifier) and compare to the emitted challenge.
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
        // base64url(no-pad) is never empty and carries no '=' padding.
        assert!(!challenge.is_empty());
        assert!(!challenge.contains('='));
    }

    #[test]
    fn build_auth_url_encodes_oauth_params() {
        let url = build_auth_url(
            "client-123",
            "http://localhost:7342/callback",
            "state-xyz",
            "chal",
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("linear.app"));
        assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            pairs.get("client_id").map(String::as_str),
            Some("client-123")
        );
        assert_eq!(
            pairs.get("redirect_uri").map(String::as_str),
            Some("http://localhost:7342/callback")
        );
        assert_eq!(pairs.get("state").map(String::as_str), Some("state-xyz"));
        assert_eq!(pairs.get("scope").map(String::as_str), Some("read,write"));
        assert_eq!(
            pairs.get("code_challenge").map(String::as_str),
            Some("chal")
        );
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
    }

    #[test]
    fn http_reply_writes_status_line_and_body() {
        let mut buf = Vec::new();
        http_reply(&mut buf, 200, "<h2>ok</h2>").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(out.contains("Content-Length: 11\r\n"));
        assert!(out.ends_with("<h2>ok</h2>"));

        let mut buf404 = Vec::new();
        http_reply(&mut buf404, 404, "nope").unwrap();
        assert!(
            String::from_utf8(buf404)
                .unwrap()
                .starts_with("HTTP/1.1 404 Not Found\r\n")
        );
    }

    // -- Token exchange (pure) ------------------------------------------------

    fn exchange<'a>() -> TokenExchange<'a> {
        TokenExchange {
            client_id: "cid",
            client_secret: "csecret",
            code: "the-code",
            redirect_uri: "http://localhost:7342/callback",
            code_verifier: "verifier",
        }
    }

    #[test]
    fn build_token_params_carries_grant_and_credentials() {
        let params = build_token_params(&exchange());
        let map: std::collections::HashMap<_, _> = params.iter().copied().collect();
        assert_eq!(map.get("grant_type").copied(), Some("authorization_code"));
        assert_eq!(map.get("client_id").copied(), Some("cid"));
        assert_eq!(map.get("client_secret").copied(), Some("csecret"));
        assert_eq!(map.get("code").copied(), Some("the-code"));
        assert_eq!(map.get("code_verifier").copied(), Some("verifier"));
    }

    #[test]
    fn parse_token_response_deserializes_success_body() {
        let token =
            parse_token_response(200, r#"{"access_token":"tok","token_type":"Bearer"}"#).unwrap();
        assert_eq!(token.access_token, "tok");
        assert_eq!(token.token_type, "Bearer");
    }

    #[test]
    fn parse_token_response_rejects_non_2xx_with_body() {
        let err = parse_token_response(400, "invalid_grant").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("HTTP 400"));
        assert!(msg.contains("invalid_grant"));
    }

    #[test]
    fn parse_token_response_rejects_malformed_json() {
        assert!(parse_token_response(200, "not json").is_err());
    }

    // -- Callback parsing (pure) ----------------------------------------------

    fn request(path_and_query: &str) -> String {
        format!("GET {path_and_query} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    #[test]
    fn parse_callback_request_returns_code_on_match() {
        let raw = request("/callback?state=st&code=abc123");
        match parse_callback_request(&raw, "st") {
            CallbackOutcome::Code(c) => assert_eq!(c, "abc123"),
            _ => panic!("expected Code"),
        }
    }

    #[test]
    fn parse_callback_request_ignores_other_paths() {
        let raw = request("/favicon.ico");
        assert!(matches!(
            parse_callback_request(&raw, "st"),
            CallbackOutcome::Ignore
        ));
    }

    #[test]
    fn parse_callback_request_detects_state_mismatch() {
        let raw = request("/callback?state=wrong&code=abc");
        assert!(matches!(
            parse_callback_request(&raw, "st"),
            CallbackOutcome::StateMismatch
        ));
    }

    #[test]
    fn parse_callback_request_surfaces_provider_error() {
        let raw = request("/callback?state=st&error=access_denied");
        match parse_callback_request(&raw, "st") {
            CallbackOutcome::Denied(e) => assert_eq!(e, "access_denied"),
            _ => panic!("expected Denied"),
        }
    }

    #[test]
    fn parse_callback_request_denies_when_no_code_or_error() {
        let raw = request("/callback?state=st");
        match parse_callback_request(&raw, "st") {
            CallbackOutcome::Denied(e) => assert_eq!(e, "unknown error"),
            _ => panic!("expected Denied"),
        }
    }

    // -- run_with_credentials end-to-end (fakes; no browser/TCP/network) ------

    struct FakeBrowser {
        opened: std::cell::RefCell<Vec<String>>,
    }
    impl Browser for FakeBrowser {
        fn open(&self, url: &str) {
            self.opened.borrow_mut().push(url.to_string());
        }
    }

    struct ScriptedCallbackListener {
        code: String,
    }
    impl CallbackListener for ScriptedCallbackListener {
        fn wait_for_code(&self, _port: u16, _expected_state: &str) -> Result<String> {
            Ok(self.code.clone())
        }
    }

    #[test]
    fn run_with_credentials_drives_the_full_flow() {
        let browser = FakeBrowser {
            opened: std::cell::RefCell::new(Vec::new()),
        };
        let listener = ScriptedCallbackListener {
            code: "auth-code".to_string(),
        };
        let exchanger = TokenExchanger::Fake {
            status: 200,
            body: r#"{"access_token":"final-token","token_type":"Bearer"}"#.to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let flow = OauthFlow {
            browser: &browser,
            listener: &listener,
            exchanger,
        };

        let token = run_with_credentials(&flow, "cid", "csecret").unwrap();
        assert_eq!(token.access_token, "final-token");

        // The browser was sent the authorization URL.
        let opened = browser.opened.borrow();
        assert_eq!(opened.len(), 1);
        assert!(opened[0].contains("linear.app"));

        // The exchange POSTed the code from the listener and our credentials.
        let TokenExchanger::Fake { calls, .. } = &flow.exchanger else {
            panic!("expected fake")
        };
        let calls = calls.borrow();
        let sent: std::collections::HashMap<_, _> = calls[0].iter().cloned().collect();
        assert_eq!(sent.get("code").map(String::as_str), Some("auth-code"));
        assert_eq!(sent.get("client_id").map(String::as_str), Some("cid"));
    }

    #[test]
    fn run_with_credentials_propagates_exchange_failure() {
        let browser = FakeBrowser {
            opened: std::cell::RefCell::new(Vec::new()),
        };
        let listener = ScriptedCallbackListener {
            code: "auth-code".to_string(),
        };
        let exchanger = TokenExchanger::Fake {
            status: 401,
            body: "unauthorized".to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let flow = OauthFlow {
            browser: &browser,
            listener: &listener,
            exchanger,
        };
        assert!(run_with_credentials(&flow, "cid", "csecret").is_err());
    }
}
