use super::{AuthStatus, Clock, SyncStatus};

/// The footer's sync/auth status label. Auth takes priority: an in-flight,
/// absent, or failed login masks whatever the sync typestate is.
pub(crate) fn sync_status_label(sync: &SyncStatus, auth: &AuthStatus, clock: &Clock) -> String {
    match auth {
        AuthStatus::Authenticating => {
            return "logging in -- complete authorization in browser".to_string();
        }
        AuthStatus::Unauthenticated | AuthStatus::Failed { .. } => {
            return "not authenticated -- press L to log in".to_string();
        }
        AuthStatus::Unknown | AuthStatus::Authenticated { .. } => {}
    }
    match sync {
        SyncStatus::Idle => "not synced".to_string(),
        SyncStatus::Syncing => "syncing...".to_string(),
        SyncStatus::Synced { synced_at } => match synced_at {
            Some(synced_at) => format_sync_label(*synced_at, clock),
            None => "not yet synced".to_string(),
        },
        SyncStatus::Failed { message, .. } => format!("sync error: {message}"),
    }
}

/// "synced X min ago" formatter -- the only branch that needs a clock.
fn format_sync_label(synced_at: chrono::DateTime<chrono::Utc>, clock: &Clock) -> String {
    let elapsed = clock.now().signed_duration_since(synced_at);
    match elapsed.num_minutes() {
        ..=0 => "synced just now".to_string(),
        mins @ 1..60 => format!("synced {mins} min ago"),
        _ => "synced over an hour ago".to_string(),
    }
}

#[cfg(all(test, feature = "fake"))]
mod tests {
    use super::{AuthStatus, Clock, SyncStatus, format_sync_label, sync_status_label};

    fn fixed_clock() -> (Clock, chrono::DateTime<chrono::Utc>) {
        let now: chrono::DateTime<chrono::Utc> = "2026-01-10T12:00:00Z".parse().unwrap_or_default();
        (Clock::Fixed(now), now)
    }

    #[test]
    fn format_sync_label_buckets_minutes_against_a_fixed_clock() {
        let (clock, now) = fixed_clock();
        let ago = |mins: i64| now - chrono::Duration::minutes(mins);

        assert_eq!(format_sync_label(now, &clock), "synced just now");
        assert_eq!(format_sync_label(ago(1), &clock), "synced 1 min ago");
        assert_eq!(format_sync_label(ago(30), &clock), "synced 30 min ago");
        // A future timestamp clamps to "just now" rather than reporting negative.
        assert_eq!(format_sync_label(ago(-5), &clock), "synced just now");
        // Past an hour, the age caps rather than reporting an unbounded count.
        assert_eq!(format_sync_label(ago(59), &clock), "synced 59 min ago");
        assert_eq!(
            format_sync_label(ago(60), &clock),
            "synced over an hour ago"
        );
        assert_eq!(
            format_sync_label(ago(120), &clock),
            "synced over an hour ago"
        );
    }

    #[test]
    fn sync_status_label_prioritizes_auth_over_sync() {
        let (clock, now) = fixed_clock();
        let unstarted_sync = SyncStatus::Idle;

        assert_eq!(
            sync_status_label(&unstarted_sync, &AuthStatus::Authenticating, &clock),
            "logging in -- complete authorization in browser"
        );
        assert_eq!(
            sync_status_label(&unstarted_sync, &AuthStatus::Unauthenticated, &clock),
            "not authenticated -- press L to log in"
        );
        assert_eq!(
            sync_status_label(
                &unstarted_sync,
                &AuthStatus::Failed {
                    message: "bad token".to_string()
                },
                &clock
            ),
            "not authenticated -- press L to log in"
        );
        assert_eq!(
            sync_status_label(&unstarted_sync, &AuthStatus::Unknown, &clock),
            "not synced"
        );
        assert_eq!(
            sync_status_label(&SyncStatus::Syncing, &AuthStatus::Unknown, &clock),
            "syncing..."
        );
        assert_eq!(
            sync_status_label(
                &SyncStatus::Synced {
                    synced_at: Some(now)
                },
                &AuthStatus::Unknown,
                &clock
            ),
            "synced just now"
        );
        assert_eq!(
            sync_status_label(
                &SyncStatus::Synced { synced_at: None },
                &AuthStatus::Unknown,
                &clock
            ),
            "not yet synced"
        );
        assert_eq!(
            sync_status_label(
                &SyncStatus::Failed {
                    message: "boom".to_string(),
                },
                &AuthStatus::Unknown,
                &clock
            ),
            "sync error: boom"
        );
    }
}
