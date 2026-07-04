/// Auto-refresh the stored OAuth session when the access token has expired.
///
/// Linear's OAuth2 token endpoint issues a refresh token alongside the access
/// token, and rotates it on every refresh (with a 30-minute grace period
/// during which the previous refresh token is still honored). This module
/// provides `load_or_refresh_token`, which:
///
///   - silently exchanges a stored refresh token for a new access token via
///     the `refresh_token` grant when the stored token has expired and a
///     refresh token is available -- no browser involved; or
///   - falls back to the full authorization-code + PKCE flow (which opens a
///     browser) when there is no token at all, no refresh token, or the
///     refresh grant itself fails.
use anyhow::{Result, anyhow};
use lt_config::AuthToken;
use tracing::{info, warn};

use super::login::{
    TOKEN_URL, TokenExchanger, UreqTokenExchanger, lookup_stored_credentials, parse_token_response,
};

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
            if let (Some(refresh_token), Some((client_id, client_secret))) =
                (t.refresh_token.clone(), lookup_stored_credentials()?)
            {
                info!("auth: access token has expired -- refreshing silently");
                match refresh_with(
                    &UreqTokenExchanger,
                    &refresh_token,
                    &client_id,
                    &client_secret,
                ) {
                    Ok(mut refreshed) => {
                        if refreshed.refresh_token.is_none() {
                            refreshed.refresh_token = Some(refresh_token);
                        }
                        lt_config::save_token(&refreshed)?;
                        return Ok(refreshed);
                    }
                    Err(e) => {
                        warn!(error = %e, "auth: silent token refresh failed -- falling back to browser login");
                    }
                }
            }

            if credentials_available()? {
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

/// Build the form fields for the refresh-token grant. Pure.
fn build_refresh_params<'a>(
    refresh_token: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
) -> [(&'a str, &'a str); 4] {
    [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ]
}

/// Exchange a refresh token for a new access token via the `refresh_token`
/// grant.
fn refresh_with(
    exchanger: &dyn TokenExchanger,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<AuthToken> {
    let params = build_refresh_params(refresh_token, client_id, client_secret);
    let (status, body) = exchanger.post_form(TOKEN_URL, &params)?;
    parse_token_response(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_refresh_params_carries_grant_and_credentials() {
        let params = build_refresh_params("rtok", "cid", "csecret");
        let map: std::collections::HashMap<_, _> = params.iter().copied().collect();
        assert_eq!(map.get("grant_type").copied(), Some("refresh_token"));
        assert_eq!(map.get("refresh_token").copied(), Some("rtok"));
        assert_eq!(map.get("client_id").copied(), Some("cid"));
        assert_eq!(map.get("client_secret").copied(), Some("csecret"));
    }

    struct FakeTokenExchanger {
        status: u16,
        body: String,
        calls: std::cell::RefCell<Vec<Vec<(String, String)>>>,
    }
    impl TokenExchanger for FakeTokenExchanger {
        fn post_form(&self, _url: &str, params: &[(&str, &str)]) -> Result<(u16, String)> {
            self.calls.borrow_mut().push(
                params
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            );
            Ok((self.status, self.body.clone()))
        }
    }

    #[test]
    fn refresh_with_returns_new_token_on_success() {
        let exchanger = FakeTokenExchanger {
            status: 200,
            body:
                r#"{"access_token":"new-tok","token_type":"Bearer","refresh_token":"new-refresh"}"#
                    .to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let token = refresh_with(&exchanger, "old-refresh", "cid", "csecret").unwrap();
        assert_eq!(token.access_token, "new-tok");
        assert_eq!(token.refresh_token, Some("new-refresh".to_string()));

        let calls = exchanger.calls.borrow();
        let sent: std::collections::HashMap<_, _> = calls[0].iter().cloned().collect();
        assert_eq!(
            sent.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            sent.get("refresh_token").map(String::as_str),
            Some("old-refresh")
        );
    }

    #[test]
    fn refresh_with_response_missing_refresh_token_is_none() {
        // The caller (load_or_refresh_token) carries over the previous
        // refresh token when the response omits it; this seam only parses
        // the response as returned.
        let exchanger = FakeTokenExchanger {
            status: 200,
            body: r#"{"access_token":"new-tok","token_type":"Bearer"}"#.to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let token = refresh_with(&exchanger, "old-refresh", "cid", "csecret").unwrap();
        assert_eq!(token.access_token, "new-tok");
        assert_eq!(token.refresh_token, None);
    }

    #[test]
    fn refresh_with_surfaces_non_2xx_error() {
        let exchanger = FakeTokenExchanger {
            status: 400,
            body: "invalid_grant".to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let err = refresh_with(&exchanger, "old-refresh", "cid", "csecret").unwrap_err();
        assert!(err.to_string().contains("invalid_grant"));
    }
}
