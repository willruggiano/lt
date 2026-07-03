// Event-loop tests
//
// These drive the DB- and event-coupled surface that the render tests skip:
// `do_fetch` and pagination against a shared in-memory SQLite, `run_app`
// driven by an `EventPump::Scripted` into a `TestBackend`, the double-esc
// reset, and the sync/login typestate transitions (`consume_sync_event`/
// `consume_login_event`) fed directly (no live threads). Writers go through
// `RecordingSyncService`, which performs the real enqueue synchronously and
// emits onto the app's queue -- `drain_events` applies it, mirroring
// `run_app`'s post-wait drain.

use std::sync::atomic::Ordering;

use lt_runtime::db::Database;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// Apply every event currently queued -- the test-side equivalent of
/// `run_app`'s post-wait drain, for handlers that write through the service
/// (whose events land on the queue, not a direct function call).
fn drain_events(app: &mut App) {
    while let Ok(event) = app.events_rx.try_recv() {
        app.apply(event);
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// A list issue with a deterministic `updated_at` so DESC ordering is stable
/// across the page boundary. State/team ids mirror their names so the
/// relational upsert reconstructs them.
fn db_issue(id: &str, ident: &str, state: &str, day: u32) -> lt_types::types::Issue {
    use lt_types::types;
    let ts = lt_types::scalars::DateTime(
        format!("2026-01-{day:02}T00:00:00Z")
            .parse()
            .unwrap_or_default(),
    );
    types::Issue {
        id: id.into(),
        identifier: ident.to_string(),
        title: format!("issue {ident}"),
        priority_label: "No priority".to_string(),
        priority: lt_types::scalars::Priority(0),
        state: types::WorkflowState {
            id: state.into(),
            name: state.to_string(),
        },
        assignee: None,
        team: types::Team {
            id: "ENG".into(),
            name: "Engineering".to_string(),
        },
        description: None,
        labels: types::IssueLabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: ts,
        updated_at: ts,
    }
}

/// A comment fixture for seeding the DB directly (bypassing the sync/outbox
/// paths) in `route_state_event` tests.
fn comment(id: &str, issue_id: &str, body: &str) -> lt_types::comments::Comment {
    let ts = lt_types::scalars::DateTime("2026-01-06T00:00:00Z".parse().unwrap_or_default());
    lt_types::comments::Comment {
        id: id.into(),
        body: body.to_string(),
        created_at: ts,
        updated_at: ts,
        user: None,
        issue_id: Some(issue_id.to_string()),
    }
}

/// Build an `App` backed by a fresh in-memory `Database` seeded with `rows`,
/// with its `RecordingSyncService` sharing that same database.
fn app_with_db(rows: &[lt_types::types::Issue]) -> Result<App> {
    let db = Database::memory()?;
    {
        let conn = db.connect()?;
        lt_runtime::db::upsert_issues(&conn, rows)?;
    }
    let mut app = App::for_test(Vec::new())?;
    app.install_db(db)?;
    Ok(app)
}

fn drive(app: &mut App, keys: &[KeyEvent]) -> Result<()> {
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut pump = EventPump::Scripted(keys.iter().copied().map(AppEvent::Key).collect());
    run_app(&mut term, &mut pump, app)
}

// -- do_fetch / pagination ------------------------------------------------

#[test]
fn do_fetch_paginated_loads_from_db() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues.len(), 3);
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1"); // updated DESC
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    assert!(!app.list_mut().pagination.has_next_page);
}

#[test]
fn do_fetch_filtered_uses_run_query() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Done", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.list_mut().filter = search_query::parse_query_ast("state:todo");
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues.len(), 2);
    assert!(app.list_mut().issues.iter().all(|i| i.state.name == "Todo"));
    // run_query has no pagination.
    assert!(!app.list_mut().pagination.has_next_page);
    assert!(app.list_mut().pagination.end_cursor.is_none());
}

#[test]
fn pending_select_seeks_identifier_on_next_issues_event() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.fetch_base_list(true);
    if let Some(list) = app.base_list_mut() {
        list.pending_select = Some("ENG-3".to_string());
    }

    app.route_state_event(&StateEvent::Issues);

    assert_eq!(app.list_mut().table_state.selected(), Some(2));
    assert!(app.list_mut().pending_select.is_none());
}

#[test]
fn next_and_prev_page_walk_offsets() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
        db_issue("4", "ENG-4", "Todo", 2),
        db_issue("5", "ENG-5", "Todo", 1),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.list_mut().args.limit = 2;
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1");
    assert!(app.list_mut().pagination.has_next_page);

    app.next_page();
    assert_eq!(
        app.list_mut().pagination.current_cursor.as_deref(),
        Some("2")
    );
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-3");

    app.prev_page();
    assert!(app.list_mut().pagination.current_cursor.is_none());
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1");
}

#[test]
fn prev_page_at_start_is_noop() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.fetch_base_list(true);
    app.prev_page(); // empty cursor stack -> no-op
    assert_eq!(app.list_mut().issues.len(), 1);
}

#[test]
fn cycle_sort_and_toggle_desc_refetch() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
    ];
    let mut app = app_with_db(&rows).unwrap();
    let before = app.list_mut().args.sort.clone();
    app.cycle_sort();
    assert_ne!(app.list_mut().args.sort, before);
    assert_eq!(app.list_mut().issues.len(), 2);

    let desc_before = app.list_mut().args.desc;
    app.toggle_desc();
    assert_ne!(app.list_mut().args.desc, desc_before);
    assert_eq!(app.list_mut().issues.len(), 2);
}

// -- populate_relations ---------------------------------------------------

#[test]
fn populate_relations_fills_parent_and_children() {
    let mut parent = db_issue("p1", "ENG-9", "Todo", 9);
    parent.title = "the parent".to_string();
    let mut child = db_issue("c1", "ENG-10", "Done", 8);
    child.parent = Some(lt_types::types::Parent {
        id: "p1".into(),
        identifier: "ENG-9".to_string(),
    });
    let app = app_with_db(&[parent, child]).unwrap();

    // The issue whose relations we resolve; populate_relations keys off its id.
    let mut issue: lt_types::types::Issue = db_issue("c1", "ENG-10", "Done", 8);
    let mut detail = build_cached_detail(&issue, Vec::new());

    // Seed the issue under a parent so query_children finds it.
    issue.id = "p1".into();
    populate_relations(&app.db, &mut detail, &issue);
    assert_eq!(detail.children.len(), 1);
    assert_eq!(detail.children[0].identifier, "ENG-10");
}

// -- route_state_event (scope-relevance matrix N1-N3, N9-N10) -------------

/// Shared N1/N3 fixture: an app seeded with `issue`, a fresh `"cm1"` comment
/// already in the DB, and a `Detail(issue)` already pushed.
fn app_with_open_detail_and_fresh_comment(
    issue: &lt_types::types::Issue,
    body: &str,
) -> Result<App> {
    let mut app = app_with_db(std::slice::from_ref(issue))?;
    let conn = app.db.connect()?;
    lt_runtime::db::upsert_comments(&conn, &[comment("cm1", issue.id.inner(), body)])?;
    drop(conn);
    app.views.push(View::Detail(Box::new(build_cached_detail(
        issue,
        Vec::new(),
    ))));
    Ok(app)
}

#[test]
fn route_state_event_comments_updates_a_live_matching_detail() {
    // N1: `Comments{A}` with `Detail(A)` live -- re-reads `query_comments(A)`.
    let issue = db_issue("c1", "ENG-1", "Todo", 5);
    let mut app = app_with_open_detail_and_fresh_comment(&issue, "fresh").unwrap();

    app.route_state_event(&StateEvent::Comments {
        issue_id: "c1".to_string(),
    });

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].body, "fresh");
}

#[test]
fn route_state_event_comments_falls_through_without_a_matching_detail() {
    // N2: no consumer at all, then a `Detail(B)` whose id does not match --
    // both drop the event.
    let a = db_issue("a", "ENG-1", "Todo", 5);
    let b = db_issue("b", "ENG-2", "Todo", 4);
    let mut app = app_with_db(&[a.clone(), b.clone()]).unwrap();
    {
        let conn = app.db.connect().unwrap();
        lt_runtime::db::upsert_comments(&conn, &[comment("cm1", "a", "fresh")]).unwrap();
    }

    // No consumer: no-op, no panic.
    app.route_state_event(&StateEvent::Comments {
        issue_id: "a".to_string(),
    });

    // Detail(b) live: id mismatch falls through.
    app.views
        .push(View::Detail(Box::new(build_cached_detail(&b, Vec::new()))));
    app.route_state_event(&StateEvent::Comments {
        issue_id: "a".to_string(),
    });
    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert!(detail.comments.is_empty());
}

#[test]
fn route_state_event_comments_applied_twice_is_idempotent() {
    // N3: duplicate/late events are idempotent re-reads of current truth.
    let issue = db_issue("c1", "ENG-1", "Todo", 5);
    let mut app = app_with_open_detail_and_fresh_comment(&issue, "fresh").unwrap();

    let ev = StateEvent::Comments {
        issue_id: "c1".to_string(),
    };
    app.route_state_event(&ev);
    app.route_state_event(&ev);

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.comments.len(), 1);
}

#[test]
fn route_state_event_issues_refreshes_the_focused_base() {
    // N9: `[List]` -- the base is focused, so `Issues` re-fetches.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues.len(), 1);

    {
        let conn = app.db.connect().unwrap();
        lt_runtime::db::upsert_issues(&conn, &[db_issue("2", "ENG-2", "Todo", 4)]).unwrap();
    }
    app.route_state_event(&StateEvent::Issues);
    assert_eq!(app.list_mut().issues.len(), 2);
}

#[test]
fn route_state_event_issues_under_an_overlay_skips_the_base_but_refreshes_detail() {
    // N10: an overlay above the base -- the base's `focused` guard drops the
    // refresh, but a live `Detail` still re-reads its own issue.
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    app.fetch_base_list(true);
    app.views.push(View::Detail(Box::new(build_cached_detail(
        &issue,
        Vec::new(),
    ))));

    let mut renamed = issue.clone();
    renamed.title = "renamed".to_string();
    {
        let conn = app.db.connect().unwrap();
        lt_runtime::db::upsert_issues(&conn, &[renamed]).unwrap();
    }
    app.route_state_event(&StateEvent::Issues);

    // The base is stale -- it never re-fetched.
    assert_eq!(app.list_mut().issues[0].title, "issue ENG-1");
    // The detail pane, being live, reflects the change immediately.
    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.issue.title, "renamed");
}

// -- optimistic writers: popup_confirm / submit_comment round trips -------

#[test]
fn popup_confirm_writes_through_the_db_and_refreshes_the_focused_base() {
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let issue_id = issue.id.inner().to_string();
    let mut app = app_with_db(&[issue]).unwrap();
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues[0].state.name, "Todo");

    app.views.push(View::Popup(PopupView {
        kind: PopupKind::State,
        issue_id,
        team_id: Some("ENG".to_string()),
        items: vec![PopupItem {
            label: "Done".to_string(),
            id: Some("done-state".to_string()),
        }],
        selected: 0,
        anchor: None,
    }));

    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    // The write goes through the service, which emits `State(Issues)` onto
    // the queue rather than routing it directly; drain it, as `run_app`
    // would in the same frame.
    drain_events(&mut app);

    // The popup pops...
    assert_eq!(app.views.len(), 1);
    // ...and the queued `Issues` invalidation re-reads the overlay-merged
    // state from the DB.
    assert_eq!(app.list_mut().issues[0].state.name, "Done");
}

#[test]
fn submit_comment_writes_through_the_db_and_refreshes_the_open_detail() {
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    let mut detail = build_cached_detail(&issue, Vec::new());
    detail.comment_input = Some("a new comment".to_string());
    app.views.push(View::Detail(Box::new(detail)));

    detail::handle_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
    );
    drain_events(&mut app);

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert!(detail.comment_input.is_none());
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].body, "a new comment");
}

// -- run_app loop ---------------------------------------------------------

#[test]
fn run_app_dispatches_keys_and_quits() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.fetch_base_list(true); // populate the list the loop renders
    drive(&mut app, &[key('j'), key('j'), key('q')]).unwrap();
    assert!(app.quit);
    assert_eq!(app.list_mut().table_state.selected(), Some(2));
}

#[test]
fn run_app_errs_when_events_exhausted_without_quit() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.fetch_base_list(true);
    // No quit key: the scripted source errors once drained, ending the loop.
    assert!(drive(&mut app, &[key('j')]).is_err());
}

// -- double-esc reset -----------------------------------------------------

#[test]
fn double_esc_resets_to_initial_filter() {
    let rows = [db_issue("1", "ENG-1", "Todo", 5)];
    let mut app = app_with_db(&rows).unwrap();
    let initial_sort = app.initial_args.sort.clone();
    let next_sort = app.list_mut().args.sort.next();
    app.list_mut().args.sort = next_sort;
    let replaced = app.list_mut().replace_sort_in_filter();
    app.list_mut().filter = replaced;
    app.last_esc_time = Some(Instant::now()); // within the 500ms window

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.list_mut().args.sort, initial_sort);
    assert!(app.last_esc_time.is_none());
}

#[test]
fn first_esc_records_timestamp() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.last_esc_time = None;
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.last_esc_time.is_some());
}

// -- typestates: consume_sync_event / consume_login_event, L/refresh -----

fn ada() -> lt_types::viewer::User {
    lt_types::viewer::User {
        id: "u1".into(),
        name: "Ada".to_string(),
        organization: lt_types::viewer::Organization {
            name: "Acme".to_string(),
            url_key: "acme".to_string(),
        },
    }
}

#[test]
fn consume_sync_event_started_sets_syncing() {
    let mut app = app_with_db(&[]).unwrap();
    app.sync = SyncStatus::Idle;

    app.consume_sync_event(SyncEvent::Started);

    assert!(matches!(app.sync, SyncStatus::Syncing));
}

#[test]
fn consume_sync_event_done_sets_identity_and_synced() {
    // N11: `Sync(Done)` only transitions the typestate now -- the `State
    // (Issues)` the loop emits alongside is a separate queued event.
    let mut app = app_with_db(&[]).unwrap();
    app.sync = SyncStatus::Syncing;

    app.consume_sync_event(SyncEvent::Done(Some(ada())));

    assert_eq!(app.auth.viewer_name(), Some("Ada"));
    assert!(matches!(app.sync, SyncStatus::Synced { .. }));
}

#[test]
fn consume_sync_event_done_without_identity_leaves_auth_unchanged() {
    let mut app = app_with_db(&[]).unwrap();
    app.auth = AuthStatus::Unauthenticated;

    app.consume_sync_event(SyncEvent::Done(None));

    assert!(matches!(app.auth, AuthStatus::Unauthenticated));
    assert!(matches!(app.sync, SyncStatus::Synced { .. }));
}

#[test]
fn consume_sync_event_done_reads_synced_at_from_db_meta() {
    let mut app = app_with_db(&[]).unwrap();
    {
        let conn = app.db.connect().unwrap();
        lt_runtime::db::set_meta(&conn, "last_synced_at", "2026-01-10T12:00:00Z").unwrap();
    }

    app.consume_sync_event(SyncEvent::Done(None));

    match &app.sync {
        SyncStatus::Synced { synced_at } => {
            assert_eq!(synced_at.to_rfc3339(), "2026-01-10T12:00:00+00:00");
        }
        SyncStatus::Idle | SyncStatus::Syncing | SyncStatus::Failed { .. } => {
            unreachable!("expected Synced")
        }
    }
}

#[test]
fn consume_sync_event_done_falls_back_to_the_clock_without_meta() {
    let mut app = app_with_db(&[]).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    app.clock = Clock::Fixed(now);

    app.consume_sync_event(SyncEvent::Done(None));

    match &app.sync {
        SyncStatus::Synced { synced_at } => assert_eq!(*synced_at, now),
        SyncStatus::Idle | SyncStatus::Syncing | SyncStatus::Failed { .. } => {
            unreachable!("expected Synced")
        }
    }
}

#[test]
fn consume_sync_event_error_sets_failed_and_repairs_a_loading_base() {
    let mut app = app_with_db(&[]).unwrap();
    app.list_mut().status = Status::Loading;

    app.consume_sync_event(SyncEvent::Error("boom".to_string()));

    match &app.sync {
        SyncStatus::Failed { message } => assert_eq!(message, "boom"),
        SyncStatus::Idle | SyncStatus::Syncing | SyncStatus::Synced { .. } => {
            unreachable!("expected Failed")
        }
    }
    assert!(matches!(app.list_mut().status, Status::Idle));
}

#[test]
fn consume_sync_event_not_authenticated_sets_auth_and_goes_idle() {
    let mut app = app_with_db(&[]).unwrap();
    app.list_mut().status = Status::Loading;

    app.consume_sync_event(SyncEvent::NotAuthenticated);

    assert!(matches!(app.auth, AuthStatus::Unauthenticated));
    assert!(matches!(app.sync, SyncStatus::Idle));
    assert!(matches!(app.list_mut().status, Status::Idle));
}

#[test]
fn consume_login_event_success_sets_identity_without_touching_sync() {
    // The follow-up delta sync after a successful login is the loop's now
    // (Decision 2); this consumer only transitions `auth`.
    let mut app = app_with_db(&[]).unwrap();
    app.sync = SyncStatus::Idle;

    app.consume_login_event(LoginEvent::Success { viewer: ada() });

    assert_eq!(app.auth.viewer_name(), Some("Ada"));
    assert!(matches!(app.sync, SyncStatus::Idle));
}

#[test]
fn consume_login_event_error_sets_failed_and_footer() {
    let mut app = app_with_db(&[]).unwrap();

    app.consume_login_event(LoginEvent::Error("bad".to_string()));

    match &app.auth {
        AuthStatus::Failed { message } => assert_eq!(message, "bad"),
        _ => unreachable!("expected Failed"),
    }
    assert!(app.footer_msg.unwrap().contains("bad"));
}

#[test]
fn l_key_gates_on_authenticating() {
    let mut app = app_with_db(&[]).unwrap();
    let db = app.db.share().unwrap();
    let service = app.install_recording_service(&db).unwrap();

    app.dispatch_key(key('L'));
    assert!(matches!(app.auth, AuthStatus::Authenticating));
    assert_eq!(service.login_calls.load(Ordering::SeqCst), 1);

    // A second press while already authenticating is a no-op -- the TUI's
    // own gate; the loop separately ignores a second `Login` command while
    // one is in flight (`lt-runtime`'s service-loop tests).
    app.dispatch_key(key('L'));
    assert_eq!(service.login_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn refresh_requests_sync_on_every_press() {
    // Item 13: `r` no longer gates on `Syncing` -- a press mid-cycle
    // coalesces into a follow-up sync instead of being ignored.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let db = app.db.share().unwrap();
    let service = app.install_recording_service(&db).unwrap();

    app.dispatch_key(key('r'));
    assert_eq!(service.request_sync_calls.load(Ordering::SeqCst), 1);

    app.dispatch_key(key('r'));
    assert_eq!(service.request_sync_calls.load(Ordering::SeqCst), 2);
}

/// Seed team `team_id` with two ordered workflow states ("Todo" @1.0, "Done"
/// @2.0), shared by the `Team{team_id}` picker-rebuild tests (N6, N8).
fn seed_team_states(conn: &lt_runtime::db::Connection, team_id: &str) -> Result<()> {
    lt_runtime::db::upsert_team_state(
        conn,
        team_id,
        &lt_types::states::WorkflowStateWithPosition {
            id: "s-todo".into(),
            name: "Todo".to_string(),
            position: 1.0,
        },
    )?;
    lt_runtime::db::upsert_team_state(
        conn,
        team_id,
        &lt_types::states::WorkflowStateWithPosition {
            id: "s-done".into(),
            name: "Done".to_string(),
            position: 2.0,
        },
    )?;
    Ok(())
}

/// A bare `NewIssueModal` for `route_state_event` tests -- only the fields
/// under test vary between callers.
fn bare_new_issue_modal() -> NewIssueModal {
    NewIssueModal {
        focused_field: NewIssueField::Team,
        title: TextInput::from(String::new()),
        description: String::new(),
        teams: Vec::new(),
        team_selected: 0,
        priorities: Vec::new(),
        priority_selected: 0,
        states: Vec::new(),
        state_selected: 0,
        assignees: Vec::new(),
        assignee_selected: 0,
        loading: true,
        error: String::new(),
        watched_team_id: None,
    }
}

#[test]
fn new_issue_modal_teams_event_rereads_and_reanchors_by_id() {
    // N4: `State(Teams)` with `NewIssue` in the stack -- re-read teams and
    // re-anchor the selection by id (not index).
    let mut app = app_with_db(&[]).unwrap();
    {
        let conn = app.db.connect().unwrap();
        lt_runtime::db::upsert_teams(
            &conn,
            &[
                lt_types::types::Team {
                    id: "t1".into(),
                    name: "Eng".to_string(),
                },
                lt_types::types::Team {
                    id: "t2".into(),
                    name: "Design".to_string(),
                },
            ],
        )
        .unwrap();
    }
    let mut modal = bare_new_issue_modal();
    modal.teams = vec![PopupItem {
        label: "Eng".to_string(),
        id: Some("t1".to_string()),
    }];
    modal.team_selected = 0;
    app.views.push(View::NewIssue(modal));

    app.route_state_event(&StateEvent::Teams);

    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert_eq!(modal.teams.len(), 2);
    // Alphabetical order puts "Design" first; the selection follows "t1" by
    // id rather than resetting to index 0.
    assert_eq!(modal.teams[modal.team_selected].id.as_deref(), Some("t1"));
}

#[test]
fn route_state_event_teams_with_no_modal_is_a_noop() {
    // N5: `State(Teams)` with no `NewIssue` in the stack -- no consumer.
    let mut app = app_with_db(&[]).unwrap();
    app.route_state_event(&StateEvent::Teams);
    assert_eq!(app.views.len(), 1);
}

#[test]
fn new_issue_modal_team_event_rereads_and_preserves_picks_by_id() {
    // N6: `State(Team{T})` with `NewIssue` on team T -- re-read states/
    // members, preserve the picks by id, clear `loading`.
    let mut app = app_with_db(&[]).unwrap();
    {
        let conn = app.db.connect().unwrap();
        seed_team_states(&conn, "t1").unwrap();
        lt_runtime::db::upsert_users(
            &conn,
            &[lt_types::types::User {
                id: "u1".into(),
                name: "Ada".to_string(),
            }],
        )
        .unwrap();
        lt_runtime::db::replace_team_memberships(&conn, "t1", &["u1"]).unwrap();
    }
    let mut modal = bare_new_issue_modal();
    modal.teams = vec![PopupItem {
        label: "Eng".to_string(),
        id: Some("t1".to_string()),
    }];
    modal.team_selected = 0;
    modal.states = vec![PopupItem {
        label: "Done".to_string(),
        id: Some("s-done".to_string()),
    }];
    modal.state_selected = 0; // "Done" picked before the refresh landed
    app.views.push(View::NewIssue(modal));

    app.route_state_event(&StateEvent::Team {
        team_id: "t1".to_string(),
    });

    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert_eq!(modal.states.len(), 2);
    // Position order puts "Todo" first; "Done" is preserved by id rather
    // than resetting to index 0.
    assert_eq!(
        modal.states[modal.state_selected].id.as_deref(),
        Some("s-done")
    );
    // "Unassigned" + the one seeded member (no synced viewer in this test).
    assert_eq!(modal.assignees.len(), 2);
    assert!(!modal.loading);
}

#[test]
fn new_issue_modal_team_event_for_a_different_team_falls_through() {
    // N7: `State(Team{T})` with `NewIssue` on team U -- the id mismatch
    // falls through, leaving `loading`/items untouched.
    let mut app = app_with_db(&[]).unwrap();
    let mut modal = bare_new_issue_modal();
    modal.teams = vec![PopupItem {
        label: "Design".to_string(),
        id: Some("t2".to_string()),
    }];
    modal.team_selected = 0;
    app.views.push(View::NewIssue(modal));

    // A refresh for a team the user has since tabbed away from.
    app.route_state_event(&StateEvent::Team {
        team_id: "t1".to_string(),
    });

    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert!(modal.states.is_empty());
    assert!(modal.loading);
}

#[test]
fn popup_team_event_rebuilds_items_and_reanchors_selection() {
    // N8: `State(Team{T})` with `Popup { team_id: Some(T) }` -- rebuild
    // `items` from the cache and re-anchor the selection by item id.
    let mut app = app_with_db(&[]).unwrap();
    {
        let conn = app.db.connect().unwrap();
        seed_team_states(&conn, "t1").unwrap();
    }
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::State,
        issue_id: "issue-1".to_string(),
        team_id: Some("t1".to_string()),
        items: vec![PopupItem {
            label: "Done".to_string(),
            id: Some("s-done".to_string()),
        }],
        selected: 0,
        anchor: None,
    }));

    app.route_state_event(&StateEvent::Team {
        team_id: "t1".to_string(),
    });

    let Some(View::Popup(popup)) = app.views.last() else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.items.len(), 2);
    // Position order puts "Todo" first; the selection is preserved by id.
    assert_eq!(popup.items[popup.selected].id.as_deref(), Some("s-done"));
}

#[test]
fn poll_search_debounce_is_noop_without_pending_change() {
    let mut app = app_with_db(&[]).unwrap();
    // No overlay -> early return.
    poll_search_debounce(&mut app);
    assert_eq!(app.views.len(), 1);
}
