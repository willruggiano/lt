// Event-loop tests: the DB- and event-coupled surface render tests skip --
// `do_fetch`/pagination, `run_app` via `EventPump::Scripted`, double-esc,
// and sync/login typestate transitions, all fed directly (no live threads).

use std::sync::atomic::Ordering;

use crossterm::event::KeyModifiers;
use lt_runtime::db::Database;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// Apply every event currently queued -- the test-side equivalent of
/// `run_app`'s post-wait drain.
fn drain_events(app: &mut App) {
    while let Ok(event) = app.events_rx.try_recv() {
        app.apply(event);
    }
}

/// Test-side re-fetch of the base list, driving the same `refetch` the
/// app's own key handlers call.
fn fetch_base_list(app: &mut App, reset_selection: bool) {
    let ctx = StateCtx {
        db: &app.db,
        viewer_name: app.auth.viewer_name(),
    };
    if let Some(View::List(list)) = app.views.first_mut() {
        list.refetch(&ctx, reset_selection);
    }
}

/// Test-side page turn, driving the same query/refetch pair as the
/// pagination arms of `apply_list`.
fn turn_page(app: &mut App, forward: bool) {
    let ctx = StateCtx {
        db: &app.db,
        viewer_name: app.auth.viewer_name(),
    };
    if let Some(View::List(list)) = app.views.first_mut() {
        let turned = if forward {
            list.query.next_page()
        } else {
            list.query.prev_page()
        };
        if turned {
            list.refetch(&ctx, true);
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
    fetch_base_list(&mut app, true);
    assert_eq!(app.list_mut().issues.len(), 3);
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1"); // updated DESC
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    assert!(!app.list_mut().query.pagination.has_next_page);
}

#[test]
fn do_fetch_filtered_uses_run_query() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Done", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.list_mut().query.filter = search_query::parse_query_ast("state:todo");
    fetch_base_list(&mut app, true);
    assert_eq!(app.list_mut().issues.len(), 2);
    assert!(app.list_mut().issues.iter().all(|i| i.state.name == "Todo"));
    // run_query has no pagination.
    assert!(!app.list_mut().query.pagination.has_next_page);
    assert!(app.list_mut().query.pagination.end_cursor.is_none());
}

#[test]
fn pending_select_seeks_identifier_on_next_issues_event() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    fetch_base_list(&mut app, true);
    app.list_mut().pending_select = Some("ENG-3".to_string());

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
    app.list_mut().query.args.limit = 2;
    fetch_base_list(&mut app, true);
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1");
    assert!(app.list_mut().query.pagination.has_next_page);

    turn_page(&mut app, true);
    assert_eq!(
        app.list_mut().query.pagination.current_cursor.as_deref(),
        Some("2")
    );
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-3");

    turn_page(&mut app, false);
    assert!(app.list_mut().query.pagination.current_cursor.is_none());
    assert_eq!(app.list_mut().issues[0].identifier, "ENG-1");
}

#[test]
fn prev_page_at_start_is_noop() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);
    turn_page(&mut app, false); // empty cursor stack -> no-op
    assert_eq!(app.list_mut().issues.len(), 1);
}

#[test]
fn toggle_desc_refetches() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
    ];
    let mut app = app_with_db(&rows).unwrap();
    let desc_before = app.list_mut().query.args.desc;
    app.dispatch_key(key('d'));
    assert_ne!(app.list_mut().query.args.desc, desc_before);
    assert_eq!(app.list_mut().issues.len(), 2);
}

// -- ListView::open ---------------------------------------------------------

#[test]
fn open_with_filterful_query_matches_post_sync_refetch() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Done", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let db = Database::memory().unwrap();
    {
        let conn = db.connect().unwrap();
        lt_runtime::db::upsert_issues(&conn, &rows).unwrap();
    }
    let ctx = StateCtx {
        db: &db,
        viewer_name: None,
    };
    let mut query = ListQuery::from(IssueQuery::default());
    query.filter = search_query::parse_query_ast("state:todo");

    // Startup: the query defines the view's initial data.
    let mut list = ListView::open(query, &ctx);
    let startup: Vec<String> = list.issues.iter().map(|i| i.identifier.clone()).collect();
    assert_eq!(startup, vec!["ENG-1".to_string(), "ENG-3".to_string()]);

    // Steady-state: the same engine, driven by the first sync's `Issues` event.
    list.consume(&ctx, true, &StateEvent::Issues);
    let post_sync: Vec<String> = list.issues.iter().map(|i| i.identifier.clone()).collect();
    assert_eq!(startup, post_sync);
}

// -- confirm_search: query handoff, not viewport-capped row transfer -------

#[test]
fn confirm_search_hands_off_the_query_not_the_viewport_capped_rows() {
    // 6 rows match state:todo; the overlay caps `results` to the 3-row
    // viewport, but the base list's query limit (the `IssueQuery` default,
    // 50) is far larger -- confirm must hand off the query so the base list
    // refetches the full match set, not the overlay's capped rows.
    let mut rows: Vec<lt_types::types::Issue> = (1..=6)
        .map(|i| db_issue(&i.to_string(), &format!("ENG-{i}"), "Todo", i))
        .collect();
    rows.push(db_issue("7", "ENG-7", "Done", 7));
    let mut app = app_with_db(&rows).unwrap();
    app.viewport_height = 3;

    let mut overlay = SearchOverlay::new();
    overlay.query = TextInput::from("state:todo".to_string());
    overlay.run_search(&app.db, app.viewport_height);
    assert_eq!(overlay.results.len(), 3); // viewport-capped
    app.views.push(View::Search(overlay));

    // Move the overlay's selection off row 0 before confirming.
    app.dispatch_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.dispatch_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let Some(View::Search(overlay)) = app.views.last() else {
        unreachable!("search view expected")
    };
    let anchor = overlay.results[overlay.table_state.selected().unwrap()]
        .identifier
        .clone();

    app.dispatch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.views.len(), 1); // overlay popped
    assert_eq!(app.list_mut().issues.len(), 6); // full match set, not capped to 3
    let selected = app.list_mut().table_state.selected().unwrap();
    assert_eq!(app.list_mut().issues[selected].identifier, anchor);
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

// -- route_state_event ------------------------------------------------------

/// An app seeded with `issue`, a fresh `"cm1"` comment already in the DB,
/// and a `Detail(issue)` already pushed.
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
    // `Comments{A}` with `Detail(A)` live re-reads `query_comments(A)`.
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
    // No consumer, then a `Detail(B)` id mismatch -- both drop the event.
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
    // Duplicate/late events are idempotent re-reads of current truth.
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
    // The base is focused, so `Issues` re-fetches.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);
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
    // An overlay above the base: the base's `focused` guard drops the
    // refresh, but a live `Detail` still re-reads its own issue.
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    fetch_base_list(&mut app, true);
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
    fetch_base_list(&mut app, true);
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

    app.dispatch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
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

    app.dispatch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
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
    fetch_base_list(&mut app, true); // populate the list the loop renders
    drive(&mut app, &[key('j'), key('j'), key('q')]).unwrap();
    assert!(app.quit);
    assert_eq!(app.list_mut().table_state.selected(), Some(2));
}

#[test]
fn run_app_errs_when_events_exhausted_without_quit() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);
    // No quit key: the scripted source errors once drained, ending the loop.
    assert!(drive(&mut app, &[key('j')]).is_err());
}

// -- double-esc reset -----------------------------------------------------

#[test]
fn double_esc_resets_to_initial_filter() {
    let rows = [db_issue("1", "ENG-1", "Todo", 5)];
    let mut app = app_with_db(&rows).unwrap();
    let initial_sort = app.list_mut().query.args.sort.clone();
    let next_sort = app.list_mut().query.args.sort.next();
    app.list_mut().query.args.sort = next_sort;
    let replaced = app.list_mut().query.replace_sort_in_filter();
    app.list_mut().query.filter = replaced;
    app.last_esc_time = Some(Instant::now()); // within the 500ms window

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.list_mut().query.args.sort, initial_sort);
    assert!(app.last_esc_time.is_none());
}

#[test]
fn first_esc_records_timestamp() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.last_esc_time = None;
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.last_esc_time.is_some());
}

// -- keymap: chords, navigation actions, esc-cancels-pending ----------------

#[test]
fn chord_g_g_selects_top() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    fetch_base_list(&mut app, true);
    app.list_mut().table_state.select(Some(2));

    app.dispatch_key(key('g'));
    assert_eq!(app.list_mut().table_state.selected(), Some(2)); // still pending

    app.dispatch_key(key('g'));
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
}

#[test]
fn chord_miss_g_j_moves_down() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
    ];
    let mut app = app_with_db(&rows).unwrap();
    fetch_base_list(&mut app, true);

    app.dispatch_key(key('g'));
    app.dispatch_key(key('j'));

    assert_eq!(app.list_mut().table_state.selected(), Some(1));
}

#[test]
fn enter_and_space_both_open_detail() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);

    app.dispatch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.views.last(), Some(View::Detail(_))));
    app.pop_view();

    app.dispatch_key(key(' '));
    assert!(matches!(app.views.last(), Some(View::Detail(_))));
}

#[test]
fn c_opens_the_create_modal() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();

    app.dispatch_key(key('c'));

    assert!(matches!(app.views.last(), Some(View::NewIssue(_))));
}

#[test]
fn esc_cancels_a_pending_chord_without_touching_last_esc_time() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.last_esc_time = None;

    app.dispatch_key(key('g'));
    assert!(app.pending_key.is_some());

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(app.pending_key.is_none());
    assert!(app.last_esc_time.is_none());
}

// -- dispatch floor: Esc/q pop overlays, reset/quit at the base -------------

/// A bare priority popup, pushed on top of `app`'s base list.
fn push_priority_popup(app: &mut App, items: Vec<PopupItem>) {
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::Priority,
        issue_id: "1".to_string(),
        team_id: None,
        items,
        selected: 0,
        anchor: None,
    }));
}

#[test]
fn floor_esc_pops_detail_overlay() {
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    app.views.push(View::Detail(Box::new(build_cached_detail(
        &issue,
        Vec::new(),
    ))));
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn floor_esc_pops_popup_overlay() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    push_priority_popup(&mut app, priority_popup_items());
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn floor_esc_pops_search_overlay() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::Search(SearchOverlay::new()));
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn floor_esc_pops_help_overlay() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::Help(HelpPopup::new()));
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn floor_esc_pops_new_issue_overlay() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::NewIssue(bare_new_issue_modal()));
    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn floor_q_pops_overlay_never_quits() {
    // `q` from an overlay is Back, never Quit.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    push_priority_popup(&mut app, priority_popup_items());
    app.dispatch_key(key('q'));
    assert_eq!(app.views.len(), 1);
    assert!(!app.quit);
}

#[test]
fn floor_q_at_base_quits() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.dispatch_key(key('q'));
    assert!(app.quit);
}

// -- cascade: unbound keys fall through toward the base; scroll and text
// contexts never cascade -----------------------------------------------------

#[test]
fn cascade_unbound_key_in_an_overlay_reaches_the_base() {
    // 'd' (toggle sort direction) is unbound in the popup's own context; it
    // should fall through the cascade to the list's binding underneath.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let before = app.list_mut().query.args.desc;
    push_priority_popup(&mut app, priority_popup_items());

    app.dispatch_key(key('d'));

    assert_ne!(app.list_mut().query.args.desc, before);
}

#[test]
fn cascade_bound_key_stops_at_the_overlay() {
    // The new-issue modal consumes every key but Esc (a text/form context);
    // `q` must not reach the base's quit.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::NewIssue(bare_new_issue_modal()));

    app.dispatch_key(key('q'));

    assert!(!app.quit);
    assert_eq!(app.views.len(), 2);
}

#[test]
fn scroll_key_moves_the_focused_view_and_never_a_view_beneath() {
    // An unconsumed scroll key resolves at the focused view's `scroll` and
    // never reaches the view beneath.
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
    ];
    let mut app = app_with_db(&rows).unwrap();
    fetch_base_list(&mut app, true);
    let base_selected_before = app.list_mut().table_state.selected();

    let issue = app.list_mut().issues[0].clone();
    app.views.push(View::Detail(Box::new(build_cached_detail(
        &issue,
        Vec::new(),
    ))));

    app.dispatch_key(key('j'));

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.scroll, 1);
    // The list beneath never saw the scroll key.
    assert_eq!(app.list_mut().table_state.selected(), base_selected_before);
}

#[test]
fn printable_key_in_a_text_context_never_cascades() {
    // `q` typed into the search query bar must be consumed as text, not
    // reach the floor's Back.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::Search(SearchOverlay::new()));

    app.dispatch_key(key('q'));

    assert_eq!(app.views.len(), 2);
    let Some(View::Search(overlay)) = app.views.last() else {
        unreachable!("search view expected")
    };
    assert!(overlay.query.value.ends_with('q'));
}

#[test]
fn printable_key_in_search_never_reaches_the_list_beneath() {
    // A printable key that means something in List ('d' toggles sort
    // direction) must stay text in the Search overlay: text contexts skip
    // GLOBAL, and forwarding always consumes, so the cascade never resumes.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let before = app.list_mut().query.args.desc;
    app.views.push(View::Search(SearchOverlay::new()));

    app.dispatch_key(key('d'));

    assert_eq!(app.list_mut().query.args.desc, before);
    let Some(View::Search(overlay)) = app.views.last() else {
        unreachable!("search view expected")
    };
    assert!(overlay.query.value.ends_with('d'));
}

#[test]
fn q_typed_in_the_help_filter_is_text() {
    // `q` above the base is normally Back at the floor; the help popup's
    // own filter bar must still get to type it.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.views.push(View::Help(HelpPopup::new()));

    app.dispatch_key(key('q'));

    assert_eq!(app.views.len(), 2);
    assert!(!app.quit);
    let Some(View::Help(popup)) = app.views.last() else {
        unreachable!("help view expected")
    };
    assert!(popup.search.value.ends_with('q'));
}

#[test]
fn esc_with_comment_input_open_cancels_input_and_keeps_detail_view() {
    // `CommentInput`'s one `esc` row: Back cancels the draft without popping
    // the Detail view beneath it -- narrower than the floor's pop.
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    let mut detail = build_cached_detail(&issue, Vec::new());
    detail.comment_input = Some("draft".to_string());
    app.views.push(View::Detail(Box::new(detail)));

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.views.len(), 2); // Detail stays open
    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert!(detail.comment_input.is_none());
}

#[test]
fn popup_scroll_supports_the_shared_motion_set() {
    // `g g`/`G`/`Ctrl-d`/`Ctrl-u`/`PageUp`/`PageDown` all move the popup
    // selection.
    let items: Vec<PopupItem> = (0..10)
        .map(|i| PopupItem {
            label: i.to_string(),
            id: None,
        })
        .collect();
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.viewport_height = 4;
    push_priority_popup(&mut app, items);

    app.dispatch_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
    let Some(View::Popup(popup)) = app.views.last() else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 9); // bottom

    // `g` is a chord prefix: two presses reach MoveTop.
    app.dispatch_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    app.dispatch_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let Some(View::Popup(popup)) = app.views.last() else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 0); // top

    app.dispatch_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    let Some(View::Popup(popup)) = app.views.last() else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 2); // half of viewport_height (4)
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
    // `Sync(Done)` only transitions the typestate; the `State(Issues)` the
    // loop emits alongside is a separate queued event.
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
fn consume_sync_event_error_sets_failed() {
    let mut app = app_with_db(&[]).unwrap();

    app.consume_sync_event(SyncEvent::Error("boom".to_string()));

    match &app.sync {
        SyncStatus::Failed { message } => assert_eq!(message, "boom"),
        SyncStatus::Idle | SyncStatus::Syncing | SyncStatus::Synced { .. } => {
            unreachable!("expected Failed")
        }
    }
}

#[test]
fn consume_sync_event_not_authenticated_sets_auth_and_goes_idle() {
    let mut app = app_with_db(&[]).unwrap();

    app.consume_sync_event(SyncEvent::NotAuthenticated);

    assert!(matches!(app.auth, AuthStatus::Unauthenticated));
    assert!(matches!(app.sync, SyncStatus::Idle));
}

#[test]
fn consume_login_event_success_sets_identity_without_touching_sync() {
    // The follow-up delta sync after a successful login is the loop's;
    // this consumer only transitions `auth`.
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
    // own gate.
    app.dispatch_key(key('L'));
    assert_eq!(service.login_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn refresh_requests_sync_on_every_press() {
    // `ctrl+r` doesn't gate on `Syncing`: a press mid-cycle coalesces into
    // a follow-up sync rather than being ignored.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let db = app.db.share().unwrap();
    let service = app.install_recording_service(&db).unwrap();

    app.dispatch_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(service.request_sync_calls.load(Ordering::SeqCst), 1);

    app.dispatch_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(service.request_sync_calls.load(Ordering::SeqCst), 2);
}

/// Seed `team_id` with two ordered workflow states ("Todo" @1.0, "Done" @2.0).
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

/// A bare `NewIssueModal` fixture; callers vary only the fields under test.
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
    // `State(Teams)` with `NewIssue` in the stack: re-read teams and
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
    // `State(Teams)` with no `NewIssue` in the stack: no consumer.
    let mut app = app_with_db(&[]).unwrap();
    app.route_state_event(&StateEvent::Teams);
    assert_eq!(app.views.len(), 1);
}

#[test]
fn new_issue_modal_team_event_rereads_and_preserves_picks_by_id() {
    // `State(Team{T})` with `NewIssue` on team T: re-read states/members,
    // preserve the picks by id, clear `loading`.
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
    // `State(Team{T})` with `NewIssue` on team U: the id mismatch falls
    // through, leaving `loading`/items untouched.
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
fn new_issue_team_picker_g_selects_the_last_team() {
    // `G` in the new-issue Team picker moves to the last item -- previously
    // a no-op via `Scroll`'s trait defaults.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let mut modal = bare_new_issue_modal();
    modal.focused_field = NewIssueField::Title;
    modal.teams = (0..5)
        .map(|i| PopupItem {
            label: format!("team-{i}"),
            id: Some(i.to_string()),
        })
        .collect();
    app.views.push(View::NewIssue(modal));

    app.dispatch_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.dispatch_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));

    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert_eq!(modal.team_selected, modal.teams.len() - 1);
}

#[test]
fn popup_team_event_rebuilds_items_and_reanchors_selection() {
    // `State(Team{T})` with `Popup { team_id: Some(T) }`: rebuild `items`
    // from the cache and re-anchor the selection by item id.
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
