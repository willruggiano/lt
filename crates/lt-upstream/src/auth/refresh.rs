/// Auto-refresh the stored OAuth session when the access token has expired.
///
/// Linear's OAuth2 token endpoint issues a refresh token alongside the access
/// token on every grant. Every stored token carries a refresh token, so
/// refreshing never depends on one being merely present. This module provides
/// `load_or_refresh_token`, which:
///
///   - silently exchanges the stored refresh token for a new access token via
///     the `refresh_token` grant when the stored token has expired and client
///     credentials are available -- no browser involved; or
///   - falls back to the full authorization-code + PKCE flow (which opens a
///     browser) when there is no token at all, no client credentials, or the
///     refresh grant itself fails.
use anyhow::{Result, anyhow};
use lt_config::AuthToken;
use tracing::{info, warn};

use super::login::{Clock, TokenExchanger, TokenRefresh, lookup_stored_credentials};

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
    let token = lt_config::load_token()?;

    match token {
        None => {
            // No token at all -- attempt login if credentials are available.
            if credentials_available()? {
                info!("auth: no token stored -- starting automatic authentication");
                super::login::run()?;
                return lt_config::load_token()?.ok_or_else(|| {
                    anyhow!("login completed but token is missing -- this is a bug")
                });
            }
            Err(anyhow!("not logged in -- run `lt auth login` first"))
        }
        Some(t) if t.is_expired() => {
            if let Some((client_id, client_secret)) = lookup_stored_credentials()? {
                info!("auth: access token has expired -- refreshing silently");
                match TokenExchanger::new(Clock::System).exchange(&TokenRefresh {
                    client_id: &client_id,
                    client_secret: &client_secret,
                    refresh_token: &t.refresh_token,
                }) {
                    Ok(refreshed) => {
                        lt_config::save_token(&refreshed)?;
                        return Ok(refreshed);
                    }
                    Err(e) => {
                        warn!(error = %e, "auth: silent token refresh failed -- falling back to browser login");
                    }
                }

                info!("auth: starting automatic re-authentication");
                super::login::run()?;
                return lt_config::load_token()?.ok_or_else(|| {
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
    Ok(lookup_stored_credentials()?.is_some())
}
