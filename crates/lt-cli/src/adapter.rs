//! The concrete [`SyncService`], backed by `lt-sync`.
//!
//! This is the only place in the TUI's runtime that touches
//! `HttpTransport`/cynic. `lt-cli` injects it into `lt_tui::run`, which lets the
//! TUI drive sync/login and modal reads without depending on `lt-sync`.

use std::sync::mpsc;

use anyhow::Result;

use lt_storage::db;
use lt_storage::query::IssueQuery;
use lt_storage::sync_port::{
    LoginEvent, Member, SyncEvent, SyncService, Team, ViewerIdentity, WorkflowState,
};
use lt_sync::client::HttpTransport;

pub struct LinearSyncService;

impl LinearSyncService {
    /// Best-effort viewer identity from the stored token.
    fn viewer_identity() -> Option<ViewerIdentity> {
        let token = lt_config::load_token().ok().flatten()?;
        let v = lt_sync::viewer::fetch_viewer(&HttpTransport::new(token.access_token)).ok()?;
        Some(ViewerIdentity {
            id: v.id,
            name: v.name,
            org_name: v.org_name,
        })
    }

    /// A transport with a fresh (auto-refreshed) token for a live read.
    fn transport() -> Result<HttpTransport> {
        let token = lt_sync::auth::refresh::load_or_refresh_token()?;
        Ok(HttpTransport::new(token.access_token))
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
                lt_sync::sync::full::run()
            } else {
                lt_sync::sync::delta::run()
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
        std::thread::spawn(move || match lt_sync::auth::login_non_interactive() {
            Ok(()) => {
                // Fetch viewer identity while the token is fresh.
                let viewer = Self::viewer_identity();
                let _ = tx.send(LoginEvent::Success {
                    viewer_name: viewer.as_ref().map(|v| v.name.clone()),
                    org_name: viewer.as_ref().map(|v| v.org_name.clone()),
                });
            }
            Err(e) => {
                let _ = tx.send(LoginEvent::Error(e.to_string()));
            }
        });
        rx
    }

    fn fetch_viewer(&self) -> Option<ViewerIdentity> {
        Self::viewer_identity()
    }

    fn fetch_teams(&self) -> Result<Vec<Team>> {
        lt_sync::mutations::fetch_teams(&Self::transport()?)
    }

    fn fetch_workflow_states(&self, team_id: &str) -> Result<Vec<WorkflowState>> {
        lt_sync::mutations::fetch_workflow_states(&Self::transport()?, team_id)
    }

    fn fetch_team_members(&self, team_id: &str) -> Result<Vec<Member>> {
        lt_sync::mutations::fetch_team_members(&Self::transport()?, team_id)
    }

    fn sync_comments(&self, issue_id: &str) -> Result<()> {
        let conn = db::open_db(db::db_path()?)?;
        lt_sync::sync::comments::sync_comments(&conn, &Self::transport()?, issue_id)
    }
}
