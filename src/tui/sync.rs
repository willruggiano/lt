use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{App, Clock, Issue, IssueArgs, LoginEvent, Mode, Status, SyncEvent};
use crate::linear::client::HttpTransport;
use crate::linear::viewer::fetch_viewer;

/// Build a human-readable "synced X min ago" or "syncing..." label, reading
/// `last_synced_at` from the database. The pure formatting lives in
/// `format_sync_label`; this is only the impure DB read.
pub(crate) fn build_sync_status_label(syncing: bool, clock: &Clock) -> String {
    if syncing {
        return format_sync_label(true, None, clock);
    }
    let last = (|| -> Option<String> {
        let conn = crate::db::open_db().ok()?;
        crate::db::get_meta(&conn, "last_synced_at").ok()?
    })();
    format_sync_label(false, last.as_deref(), clock)
}

/// Pure formatter for the sync status label. `clock` is read only when a
/// `last_synced` timestamp parses, so the syncing / not-synced / parse-error
/// branches never touch it -- which is what keeps the binary's wall clock and
/// the tests' fixed clock interchangeable here.
fn format_sync_label(syncing: bool, last_synced: Option<&str>, clock: &Clock) -> String {
    if syncing {
        return "syncing...".to_string();
    }
    match last_synced {
        None => "not synced".to_string(),
        Some(ts) => {
            // Parse RFC3339 and compute elapsed minutes.
            match chrono::DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => {
                    let elapsed = clock
                        .now()
                        .signed_duration_since(dt.with_timezone(&chrono::Utc));
                    let mins = elapsed.num_minutes();
                    match mins {
                        ..=0 => "synced just now".to_string(),
                        1 => "synced 1 min ago".to_string(),
                        _ => format!("synced {mins} min ago"),
                    }
                }
                Err(_) => "synced".to_string(),
            }
        }
    }
}

/// Spawn a background sync thread and return the receiver.
///
/// When `full` is true the thread runs a full sync (re-fetches every issue);
/// otherwise it runs a delta sync (only issues updated since last sync).
///
/// When `fetch_identity` is true the thread also fetches the viewer identity
/// after a successful sync and includes it in `SyncEvent::Done`.  This keeps
/// the header current when authentication happened outside the TUI's own
/// login flow -- e.g. the sync's automatic re-auth, or `lt auth login` run in
/// another terminal.
pub(crate) fn spawn_sync_thread(
    args: IssueArgs,
    full: bool,
    fetch_identity: bool,
) -> mpsc::Receiver<SyncEvent> {
    let (tx, rx) = mpsc::channel::<SyncEvent>();
    std::thread::spawn(move || {
        // Skip sync when no auth token is stored; notify the TUI.
        match crate::config::load_token() {
            Ok(None) | Err(_) => {
                let _ = tx.send(SyncEvent::NotAuthenticated);
                return;
            }
            Ok(Some(_)) => {}
        }

        // Run the requested sync variant.
        let result = if full {
            crate::sync::full::run()
        } else {
            crate::sync::delta::run()
        };
        match result {
            Ok(()) => {
                // Re-query SQLite for a fresh issue list to send to TUI.
                let issues = (|| -> Result<Vec<Issue>> {
                    let conn = crate::db::open_db()?;
                    let db_issues = crate::db::query_issues(&conn, &args)?;
                    // Convert db::Issue -> issues::list::Issue.
                    Ok(db_issues.into_iter().map(Into::into).collect())
                })();
                // A successful sync implies a valid token, so the identity
                // fetch is expected to succeed; failures leave the header
                // unchanged and the next sync retries.
                let viewer = if fetch_identity {
                    crate::config::load_token()
                        .ok()
                        .flatten()
                        .and_then(|t| fetch_viewer(&HttpTransport::new(t.access_token)).ok())
                } else {
                    None
                };
                match issues {
                    Ok(list) => {
                        let _ = tx.send(SyncEvent::Done(list, viewer));
                    }
                    Err(e) => {
                        let _ = tx.send(SyncEvent::Error(e.to_string()));
                    }
                }
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

/// Spawn a background thread that runs the non-interactive OAuth login flow.
pub(crate) fn spawn_login_thread() -> mpsc::Receiver<LoginEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || match crate::auth::login_non_interactive() {
        Ok(()) => {
            // Fetch viewer identity while the token is fresh.
            let viewer = crate::config::load_token()
                .ok()
                .flatten()
                .and_then(|t| fetch_viewer(&HttpTransport::new(t.access_token)).ok());
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

/// Poll the background login channel and update app state on completion.
pub(crate) fn poll_login_events(app: &mut App) {
    let Some(rx) = app.login_rx.as_ref() else {
        return;
    };
    match rx.try_recv() {
        Ok(LoginEvent::Success {
            viewer_name,
            org_name,
        }) => {
            app.login_rx = None;
            if let Some(name) = viewer_name {
                app.viewer_name = Some(name);
            }
            if let Some(org) = org_name {
                app.org_name = Some(org);
            }
            app.session.not_authenticated = false;
            app.sync.syncing = true;
            app.sync.sync_status_label = build_sync_status_label(true, &app.clock);
            app.sync.sync_rx = Some(spawn_sync_thread(
                app.args.clone(),
                false,
                app.viewer_name.is_none(),
            ));
        }
        Ok(LoginEvent::Error(msg)) => {
            app.login_rx = None;
            app.footer_msg = Some(format!("Login failed: {msg}"));
            app.sync.sync_status_label = "not authenticated -- press L to log in".to_string();
        }
        Err(mpsc::TryRecvError::Empty) => {} // still waiting
        Err(mpsc::TryRecvError::Disconnected) => {
            app.login_rx = None;
        }
    }
}

/// Non-blocking poll of the background sync channel.
pub(crate) fn poll_sync_events(app: &mut App) {
    // Take the receiver out temporarily so we can mutate app freely.
    let Some(rx) = app.sync.sync_rx.take() else {
        return;
    };

    let mut got_event = false;
    loop {
        match rx.try_recv() {
            Ok(SyncEvent::Done(_new_issues, viewer)) => {
                // Update the header identity when the sync thread fetched it
                // (authentication happened outside the L-key login flow).
                if let Some(v) = viewer {
                    app.viewer_name = Some(v.name);
                    app.org_name = Some(v.org_name);
                    app.session.not_authenticated = false;
                }
                // Sync finished: refresh the issue list from SQLite so that
                // has_next_page and end_cursor are recalculated correctly.
                // Only refresh if the user is in normal list mode on page 1.
                if matches!(app.mode, Mode::List)
                    && app.pagination.cursor_stack.is_empty()
                    && app.pagination.current_cursor.is_none()
                {
                    app.do_fetch(false);
                }
                app.sync.syncing = false;
                app.sync.sync_status_label = build_sync_status_label(false, &app.clock);
                // Schedule next periodic delta sync in 30s.
                app.sync.next_sync_at = Some(Instant::now() + Duration::from_secs(30));
                got_event = true;
            }
            Ok(SyncEvent::Error(msg)) => {
                app.sync.syncing = false;
                app.sync.sync_status_label = format!("sync error: {msg}");
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                // Retry periodic sync in 30s even after errors.
                app.sync.next_sync_at = Some(Instant::now() + Duration::from_secs(30));
                got_event = true;
            }
            Ok(SyncEvent::NotAuthenticated) => {
                app.sync.syncing = false;
                app.session.not_authenticated = true;
                app.sync.sync_status_label = "not authenticated -- press L to log in".to_string();
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                // Don't schedule periodic sync when not authenticated.
                app.sync.next_sync_at = None;
                got_event = true;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                app.sync.syncing = false;
                if app.sync.sync_status_label == "syncing..." {
                    app.sync.sync_status_label = build_sync_status_label(false, &app.clock);
                }
                got_event = true;
                break;
            }
        }
    }

    // Put the receiver back if the thread may still send more messages.
    if !got_event || app.sync.syncing {
        app.sync.sync_rx = Some(rx);
    }
}

#[cfg(all(test, feature = "sim"))]
mod tests {
    use super::{Clock, format_sync_label};

    #[test]
    fn format_sync_label_buckets_minutes_against_a_fixed_clock() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let clock = Clock::Fixed(now);
        // `syncing` short-circuits regardless of the timestamp or clock.
        assert_eq!(
            format_sync_label(true, Some("2026-01-10T11:00:00Z"), &clock),
            "syncing..."
        );
        // No recorded sync, and an unparseable timestamp, degrade gracefully.
        assert_eq!(format_sync_label(false, None, &clock), "not synced");
        assert_eq!(
            format_sync_label(false, Some("not-a-date"), &clock),
            "synced"
        );
        // Elapsed-minute buckets, all measured against the fixed clock.
        assert_eq!(
            format_sync_label(false, Some("2026-01-10T12:00:00Z"), &clock),
            "synced just now"
        );
        assert_eq!(
            format_sync_label(false, Some("2026-01-10T11:59:00Z"), &clock),
            "synced 1 min ago"
        );
        assert_eq!(
            format_sync_label(false, Some("2026-01-10T11:30:00Z"), &clock),
            "synced 30 min ago"
        );
        // A future timestamp clamps to "just now" rather than reporting negative.
        assert_eq!(
            format_sync_label(false, Some("2026-01-10T12:05:00Z"), &clock),
            "synced just now"
        );
    }
}
