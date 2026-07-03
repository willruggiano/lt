// Event-loop tests
//
// These drive the DB- and event-coupled surface that the render tests skip:
// `do_fetch` and pagination against a shared in-memory SQLite, `run_app`
// driven by a scripted `EventSource` into a `TestBackend`, the double-esc
// reset, and the background-channel pollers fed directly (no live threads).
// Per the agreed scope, no network-spawning method is invoked.

use std::collections::VecDeque;

use lt_runtime::db::Database;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// Scripted key source for `run_app`. Yields the queued keys, then errors so
/// a forgotten quit key terminates the loop instead of hanging.
struct ScriptedEvents {
    keys: VecDeque<KeyEvent>,
}

impl EventSource for ScriptedEvents {
    fn next_key(&mut self, _timeout: Duration) -> Result<Option<KeyEvent>> {
        match self.keys.pop_front() {
            Some(k) => Ok(Some(k)),
            None => Err(anyhow::anyhow!("scripted events exhausted")),
        }
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

/// Build an `App` backed by a fresh in-memory `Database` seeded with `rows`.
fn app_with_db(rows: &[lt_types::types::Issue]) -> Result<App> {
    let db = Database::memory()?;
    {
        let conn = db.connect()?;
        lt_runtime::db::upsert_issues(&conn, rows)?;
    }
    let mut app = App::for_test(Vec::new());
    app.db = db;
    Ok(app)
}

fn drive(app: &mut App, keys: &[KeyEvent]) -> Result<()> {
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = ScriptedEvents {
        keys: keys.iter().copied().collect(),
    };
    run_app(&mut term, &mut events, app)
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
    app.active_filter = search_query::parse_query_ast("state:todo");
    app.fetch_base_list(true);
    assert_eq!(app.list_mut().issues.len(), 2);
    assert!(app.list_mut().issues.iter().all(|i| i.state.name == "Todo"));
    // run_query has no pagination.
    assert!(!app.list_mut().pagination.has_next_page);
    assert!(app.list_mut().pagination.end_cursor.is_none());
}

#[test]
fn do_fetch_and_select_seeks_identifier() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    let ctx = StateCtx {
        db: &app.db,
        args: &app.args,
        filter: &app.active_filter,
        viewer_name: app.viewer_name.as_deref(),
    };
    if let Some(View::List(list)) = app.views.first_mut() {
        list.do_fetch_and_select(&ctx, Some("ENG-3".to_string()));
    }
    assert_eq!(app.list_mut().table_state.selected(), Some(2));
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
    app.args.limit = 2;
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
    let before = app.args.sort.clone();
    app.cycle_sort();
    assert_ne!(app.args.sort, before);
    assert_eq!(app.list_mut().issues.len(), 2);

    let desc_before = app.args.desc;
    app.toggle_desc();
    assert_ne!(app.args.desc, desc_before);
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

    // The popup pops...
    assert_eq!(app.views.len(), 1);
    // ...and the routed `Issues` invalidation re-reads the overlay-merged
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
    app.args.sort = app.args.sort.next();
    app.active_filter = app.replace_sort_in_filter();
    app.last_esc_time = Some(Instant::now()); // within the 500ms window

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.args.sort, initial_sort);
    assert!(app.last_esc_time.is_none());
}

#[test]
fn first_esc_records_timestamp() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.last_esc_time = None;
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.last_esc_time.is_some());
}

// -- background-channel pollers (fed directly, no live threads) -----------

#[test]
fn poll_sync_events_handles_not_authenticated() {
    let mut app = app_with_db(&[]).unwrap();
    let (tx, rx) = mpsc::channel();
    tx.send(SyncEvent::NotAuthenticated).unwrap();
    app.sync.sync_rx = Some(rx);
    poll_sync_events(&mut app);
    assert!(app.session.not_authenticated);
    assert!(!app.sync.syncing);
    assert!(app.sync.next_sync_at.is_none());
}

#[test]
fn poll_sync_events_handles_error() {
    let mut app = app_with_db(&[]).unwrap();
    app.sync.syncing = true;
    let (tx, rx) = mpsc::channel();
    tx.send(SyncEvent::Error("boom".to_string())).unwrap();
    app.sync.sync_rx = Some(rx);
    poll_sync_events(&mut app);
    assert!(!app.sync.syncing);
    assert!(app.sync.sync_status_label.contains("boom"));
}

#[test]
fn poll_sync_events_done_refreshes_and_sets_identity() {
    let rows = [db_issue("1", "ENG-1", "Todo", 5)];
    let mut app = app_with_db(&rows).unwrap();
    app.sync.syncing = true;
    let (tx, rx) = mpsc::channel();
    tx.send(SyncEvent::Done(Some(lt_types::viewer::User {
        id: "u1".into(),
        name: "Ada".to_string(),
        organization: lt_types::viewer::Organization {
            name: "Acme".to_string(),
            url_key: "acme".to_string(),
        },
    })))
    .unwrap();
    app.sync.sync_rx = Some(rx);
    poll_sync_events(&mut app);
    assert_eq!(app.viewer_name.as_deref(), Some("Ada"));
    assert!(!app.sync.syncing);
    assert!(app.sync.next_sync_at.is_some());
    assert_eq!(app.list_mut().issues.len(), 1); // do_fetch ran against the cache
}

#[test]
fn poll_login_events_error_sets_footer() {
    let mut app = app_with_db(&[]).unwrap();
    let (tx, rx) = mpsc::channel();
    tx.send(LoginEvent::Error("bad".to_string())).unwrap();
    app.login_rx = Some(rx);
    poll_login_events(&mut app);
    assert!(app.login_rx.is_none());
    assert!(app.footer_msg.unwrap().contains("bad"));
}

#[test]
fn poll_login_events_disconnected_clears_receiver() {
    let mut app = app_with_db(&[]).unwrap();
    let (tx, rx) = mpsc::channel::<LoginEvent>();
    drop(tx);
    app.login_rx = Some(rx);
    poll_login_events(&mut app);
    assert!(app.login_rx.is_none());
}

#[test]
fn poll_modal_events_applies_loaded_data() {
    let mut app = app_with_db(&[]).unwrap();
    let (tx, rx) = mpsc::channel();
    tx.send(ModalEvent::StatesLoaded(vec![PopupItem {
        label: "Todo".to_string(),
        id: Some("s1".to_string()),
    }]))
    .unwrap();
    tx.send(ModalEvent::AssigneesLoaded(vec![PopupItem {
        label: "Ada".to_string(),
        id: Some("u1".to_string()),
    }]))
    .unwrap();
    app.views.push(View::NewIssue(NewIssueModal {
        focused_field: NewIssueField::Title,
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
        modal_rx: Some(rx),
    }));
    app.poll_modal_events();
    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert_eq!(modal.states.len(), 1);
    assert_eq!(modal.assignees.len(), 1);
    assert!(!modal.loading);
}

#[test]
fn poll_search_debounce_is_noop_without_pending_change() {
    let mut app = app_with_db(&[]).unwrap();
    // No overlay -> early return.
    poll_search_debounce(&mut app);
    assert_eq!(app.views.len(), 1);
}
