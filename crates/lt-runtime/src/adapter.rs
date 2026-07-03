//! The concrete [`SyncService`], backed by `lt-upstream`.
//!
//! This is the only place in the TUI's runtime that touches
//! `HttpTransport`/cynic. `lt-cli` injects it into `tui::run`, which lets the
//! TUI drive sync/login and modal reads without depending on `lt-upstream`.

use std::sync::mpsc;

use anyhow::Result;
use lt_storage::db;
use lt_types::query::IssueQuery;
use lt_types::viewer;
use lt_types::viewer::ViewerQuery;
use lt_upstream::auth::login_non_interactive;
use lt_upstream::auth::refresh::load_or_refresh_token;
use lt_upstream::client::{HttpTransport, execute};
use rusqlite::Connection;

use crate::sync::service::{LoginEvent, SyncEvent, SyncService};

pub struct LinearSyncService;

impl LinearSyncService {
    /// Best-effort viewer identity from the stored token.
    fn viewer_identity() -> Option<viewer::User> {
        let token = lt_config::load_token().ok().flatten()?;
        execute::<ViewerQuery>(&HttpTransport::new(token.access_token), ()).ok()
    }

    /// A transport with a fresh (auto-refreshed) token for a live read.
    fn transport() -> Result<HttpTransport> {
        let token = load_or_refresh_token()?;
        Ok(HttpTransport::new(token.access_token))
    }

    /// The shared shape behind every targeted API-to-DB writer
    /// (`sync_comments`/`sync_teams`/`sync_team_data`): open the profile DB,
    /// build a fresh transport, then run `f`.
    fn sync_with(f: impl FnOnce(&Connection, &HttpTransport) -> Result<()>) -> Result<()> {
        let conn = db::open_db(db::db_path()?)?;
        f(&conn, &Self::transport()?)
    }
}

impl SyncService for LinearSyncService {
    fn spawn_sync(
        &self,
        _query: IssueQuery,
        full: bool,
        fetch_identity: bool,
    ) -> mpsc::Receiver<SyncEvent> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Skip sync when no auth token is stored; notify the TUI.
            match lt_config::load_token() {
                Ok(None) | Err(_) => {
                    let _ = tx.send(SyncEvent::NotAuthenticated);
                    return;
                }
                Ok(Some(_)) => {}
            }

            let result = if full {
                crate::sync::full::run()
            } else {
                crate::sync::delta::run()
            };
            match result {
                Ok(()) => {
                    // A successful sync implies a valid token, so the identity
                    // fetch is expected to succeed; failures leave the header
                    // unchanged and the next sync retries.
                    let viewer = if fetch_identity {
                        Self::viewer_identity()
                    } else {
                        None
                    };
                    let _ = tx.send(SyncEvent::Done(viewer));
                }
                Err(e) => {
                    // Surface only the outermost error message to keep the
                    // statusbar readable (the anyhow chain can be very long).
                    let msg = e.to_string();
                    let brief = msg.lines().next().unwrap_or(&msg).to_string();
                    let _ = tx.send(SyncEvent::Error(brief));
                }
            }
        });
        rx
    }

    fn spawn_login(&self) -> mpsc::Receiver<LoginEvent> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || match login_non_interactive() {
            Ok(()) => {
                // Fetch viewer identity while the token is fresh.
                let viewer = Self::viewer_identity();
                let _ = tx.send(LoginEvent::Success {
                    viewer_name: viewer.as_ref().map(|v| v.name.clone()),
                    org_name: viewer.as_ref().map(|v| v.organization.name.clone()),
                });
            }
            Err(e) => {
                let _ = tx.send(LoginEvent::Error(e.to_string()));
            }
        });
        rx
    }

    fn fetch_viewer(&self) -> Option<viewer::User> {
        Self::viewer_identity()
    }

    fn sync_comments(&self, issue_id: &str) -> Result<()> {
        Self::sync_with(|conn, transport| crate::comments::sync(conn, transport, issue_id))
    }

    fn sync_teams(&self) -> Result<()> {
        Self::sync_with(|conn, transport| crate::teams::sync_teams(conn, transport))
    }

    fn sync_team_data(&self, team_id: &str) -> Result<()> {
        Self::sync_with(|conn, transport| crate::teams::sync_team_data(conn, transport, team_id))
    }
}
