/// Auto-refresh the stored OAuth session when the access token has expired.
///
/// Linear's OAuth2 implementation does not issue refresh tokens.  The only
/// way to obtain a new access token is to run the full authorization-code +
/// PKCE flow again.  This module provides `try_refresh`, which attempts that
/// flow when the stored token appears to be expired AND the client credentials
/// (client_id + client_secret) are available -- either from environment
/// variables or from the stored config file.
///
/// The function is best-effort:
///   - Returns Ok(false) when refresh is not possible (no credentials, no
///     expiry information, or the token is still valid).
///   - Returns Ok(true) when a fresh token was obtained and saved.
///   - Returns Err(...) only when credentials are present and the OAuth flow
///     itself failed.
use anyhow::{Result, anyhow};
use tracing::info;

use crate::config;
use crate::config::AuthToken;

/// Attempt to obtain a fresh access token if the current one has expired.
///
/// Returns `true` when a new token was saved, `false` when no refresh was
/// needed or possible.
#[allow(dead_code)]
pub fn try_refresh() -> Result<bool> {
    // Load the current token.  If there is none, nothing to refresh.
    let token = match config::load_token()? {
        Some(t) => t,
        None => return Ok(false),
    };

    // Only attempt refresh when the token is known to have expired.
    if !token.is_expired() {
        return Ok(false);
    }

    // Check whether we have the client credentials needed to drive the OAuth
    // flow.  Without them we cannot open the authorization URL.
    if !credentials_available()? {
        info!("auth: token is expired but no client credentials are stored -- cannot auto-refresh");
        return Ok(false);
    }

    info!("auth: access token has expired -- starting automatic re-authentication");

    // Delegate to the standard login flow, which stores the new token.
    super::login::run()?;

    Ok(true)
}

/// Load the stored token, attempting an automatic re-authentication if the
/// token is expired and client credentials are available.
///
/// This is the preferred entry-point for commands that need a valid token
/// but want transparent session renewal rather than a hard "not logged in"
/// error.
///
/// Returns `Err` when:
///   - No token is stored and re-auth was not possible.
///   - The token is expired, credentials are available, but the OAuth flow
///     failed.
pub fn load_or_refresh_token() -> Result<AuthToken> {
    let token = config::load_token()?;

    match token {
        None => {
            // No token at all -- attempt login if credentials are available.
            if credentials_available()? {
                info!("auth: no token stored -- starting automatic authentication");
                super::login::run()?;
                return config::load_token()?.ok_or_else(|| {
                    anyhow!("login completed but token is missing -- this is a bug")
                });
            }
            Err(anyhow!("not logged in -- run `lt auth login` first"))
        }
        Some(t) if t.is_expired() => {
            if credentials_available()? {
                info!("auth: access token has expired -- starting automatic re-authentication");
                super::login::run()?;
                return config::load_token()?.ok_or_else(|| {
                    anyhow!("re-auth completed but token is missing -- this is a bug")
                });
            }
            // Credentials not available: return the expired token and let the
            // API surface the 401 with a clear message.
            info!("auth: token is expired but no client credentials found; using stale token");
            Ok(t)
        }
        Some(t) => Ok(t),
    }
}

/// Return true when OAuth client credentials are resolvable (env vars or
/// config file), without blocking for interactive input.
fn credentials_available() -> Result<bool> {
    // Environment variables.
    if std::env::var("LINEAR_CLIENT_ID").is_ok() && std::env::var("LINEAR_CLIENT_SECRET").is_ok() {
        return Ok(true);
    }

    // Stored config file.
    let cfg = config::load_config()?;
    Ok(cfg.client_id.is_some() && cfg.client_secret.is_some())
}
