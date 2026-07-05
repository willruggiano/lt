// Event-loop tests: the DB- and event-coupled surface render tests skip --
// `do_fetch`/pagination, `run_app` via `EventPump::Scripted`, double-esc,
// and sync/login typestate transitions, all fed directly (no live threads).
//
// Tests drive the real `Runtime` over an in-memory `Database`, never
// starting `run()` (docs/design/operation-seam-adr.md, "Decision 7"):
// initial reads and write propagation are synchronous. Live-update routing
// for Teams/WorkflowStates/TeamMemberships-scoped subscriptions (the
// state/assignee popup, the new-issue modal) is exercised at the
// `lt-runtime` layer instead (`crates/lt-runtime/src/runtime.rs`), since
// reaching it needs an upstream refresh no `Runtime` write method can
// synthesize; these tests cover their construction, the team-change
// drop/resubscribe, and the pure `reanchor` helper.

use crossterm::event::KeyModifiers;
use lt_runtime::test_util::Database;
use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
use lt_types::teams::TeamsQuery;
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

/// Test-side resubscribe of the base list, driving the same `resubscribe`
/// the app's own key handlers call.
fn fetch_base_list(app: &mut App, reset_selection: bool) {
    let viewer_name = app.auth.viewer_name().map(str::to_string);
    let runtime = app.runtime.clone();
    if let Some(View::List(list)) = app.views.first_mut() {
        list.resubscribe(&runtime, viewer_name.as_deref(), reset_selection);
    }
}

/// Test-side page turn, driving the same query/resubscribe pair as the
/// pagination arms of `apply_list`.
fn turn_page(app: &mut App, forward: bool) {
    let viewer_name = app.auth.viewer_name().map(str::to_string);
    let runtime = app.runtime.clone();
    if let Some(View::List(list)) = app.views.first_mut() {
        let turned = if forward {
            list.query.next_page()
        } else {
            list.query.prev_page()
        };
        if turned {
            list.resubscribe(&runtime, viewer_name.as_deref(), true);
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
            position: None,
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

/// Build an `App` backed by a fresh in-memory `Database` seeded with `rows`,
/// with its `Runtime` sharing that same database, returning the database
/// handle too so a test can seed further rows later (shared-cache: any
/// connection off this handle reaches the same rows the app's `Runtime`
/// reads/writes).
fn app_with_db_and_handle(rows: &[lt_types::types::Issue]) -> Result<(App, Database)> {
    let db = Database::memory()?;
    {
        let conn = db.connect()?;
        lt_runtime::test_util::upsert_issues(&conn, rows)?;
    }
    let mut app = App::for_test(Vec::new())?;
    app.install_db(&db)?;
    Ok((app, db))
}

/// [`app_with_db_and_handle`], for the (common) case where the test never
/// needs to seed further rows after construction.
fn app_with_db(rows: &[lt_types::types::Issue]) -> Result<App> {
    Ok(app_with_db_and_handle(rows)?.0)
}

/// Push a `Detail` view for `issue`, subscribing its comment thread.
fn open_detail_for(app: &mut App, issue: &lt_types::types::Issue) {
    let detail = build_cached_detail(issue, &app.runtime);
    app.views.push(View::Detail(Box::new(detail)));
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
fn do_fetch_filtered_uses_the_merged_read() {
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
    // Fewer matches than the default page size: no next page.
    assert!(!app.list_mut().query.pagination.has_next_page);
    assert!(app.list_mut().query.pagination.end_cursor.is_none());
}

#[test]
fn pending_select_seeks_identifier_on_next_issues_update() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    fetch_base_list(&mut app, true);
    app.list_mut().pending_select = Some("ENG-3".to_string());

    // A write that touches `Issue` propagates to the live list subscription.
    app.runtime
        .edit_issue("1", lt_runtime::sync::service::IssueEdit::Priority(0))
        .unwrap();
    drain_events(&mut app);

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
    app.list_mut().query.limit = 2;
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
    let direction_before = app.list_mut().query.order.direction;
    app.dispatch_key(key('d'));
    assert_ne!(app.list_mut().query.order.direction, direction_before);
    assert_eq!(app.list_mut().issues.len(), 2);
}

// -- ListView::open ---------------------------------------------------------

#[test]
fn open_with_filterful_query_matches_post_sync_resubscribe() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Done", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let db = Database::memory().unwrap();
    {
        let conn = db.connect().unwrap();
        lt_runtime::test_util::upsert_issues(&conn, &rows).unwrap();
    }
    let (tx, _rx) = mpsc::channel();
    let runtime = test_runtime(db, tx);
    let mut query = ListQuery::new(
        search_query::parse_query_ast(search_query::DEFAULT_QUERY),
        50,
    );
    query.filter = search_query::parse_query_ast("state:todo");

    // Startup: the query defines the view's initial data.
    let mut list = ListView::open(query, &runtime, None);
    let startup: Vec<String> = list.issues.iter().map(|i| i.identifier.clone()).collect();
    assert_eq!(startup, vec!["ENG-1".to_string(), "ENG-3".to_string()]);

    // Steady-state: the same engine, driven by a resubscribe (what a sync's
    // propagation triggers via the subscription's own re-read closure).
    list.resubscribe(&runtime, None, true);
    let post_sync: Vec<String> = list.issues.iter().map(|i| i.identifier.clone()).collect();
    assert_eq!(startup, post_sync);
}

// -- confirm_search: query handoff, not viewport-capped row transfer -------

#[test]
fn confirm_search_hands_off_the_query_not_the_viewport_capped_rows() {
    // 6 rows match state:todo; the overlay caps `results` to the 3-row
    // viewport, but the base list's query limit (the default, 50) is far
    // larger -- confirm must hand off the query so the base list refetches
    // the full match set, not the overlay's capped rows.
    let mut rows: Vec<lt_types::types::Issue> = (1..=6)
        .map(|i| db_issue(&i.to_string(), &format!("ENG-{i}"), "Todo", i))
        .collect();
    rows.push(db_issue("7", "ENG-7", "Done", 7));
    let mut app = app_with_db(&rows).unwrap();
    app.viewport_height = 3;

    let mut overlay = SearchOverlay::new();
    overlay.query = TextInput::from("state:todo".to_string());
    overlay.run_search(&app.runtime, app.viewport_height);
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

// -- build_cached_detail: children come through the composed subscription --

#[test]
fn build_cached_detail_populates_children_from_the_subscription() {
    let mut parent = db_issue("p1", "ENG-9", "Todo", 9);
    parent.title = "the parent".to_string();
    let mut child = db_issue("c1", "ENG-10", "Done", 8);
    child.parent = Some(lt_types::types::Parent {
        id: "p1".into(),
        identifier: "ENG-9".to_string(),
    });
    let app = app_with_db(&[parent.clone(), child]).unwrap();

    let detail = build_cached_detail(&parent, &app.runtime);

    assert_eq!(detail.children.len(), 1);
    assert_eq!(detail.children[0].identifier, "ENG-10");
}

// -- route_update: comments ---------------------------------------------

#[test]
fn route_update_comments_updates_a_live_matching_detail() {
    let issue = db_issue("c1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    open_detail_for(&mut app, &issue);

    app.runtime
        .create_comment(&CommentCreateInput {
            issue_id: "c1".to_string(),
            body: "fresh".to_string(),
        })
        .unwrap();
    drain_events(&mut app);

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].body, "fresh");
}

#[test]
fn route_update_comments_falls_through_without_a_matching_detail() {
    let a = db_issue("a", "ENG-1", "Todo", 5);
    let b = db_issue("b", "ENG-2", "Todo", 4);
    let mut app = app_with_db(&[a.clone(), b.clone()]).unwrap();

    // No consumer yet: no-op, no panic.
    app.runtime
        .create_comment(&CommentCreateInput {
            issue_id: "a".to_string(),
            body: "fresh".to_string(),
        })
        .unwrap();
    drain_events(&mut app);

    // Detail(b) live: id mismatch falls through.
    open_detail_for(&mut app, &b);
    app.runtime
        .create_comment(&CommentCreateInput {
            issue_id: "a".to_string(),
            body: "fresh2".to_string(),
        })
        .unwrap();
    drain_events(&mut app);

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert!(detail.comments.is_empty());
}

#[test]
fn route_update_comments_applied_twice_is_idempotent() {
    // Duplicate/late events are idempotent: the second `take()` finds
    // nothing new.
    let issue = db_issue("c1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    open_detail_for(&mut app, &issue);

    app.runtime
        .create_comment(&CommentCreateInput {
            issue_id: "c1".to_string(),
            body: "fresh".to_string(),
        })
        .unwrap();
    let ev = app.events_rx.recv().unwrap();
    let AppEvent::Runtime(RuntimeEvent::Updated(id)) = ev else {
        unreachable!("expected an Updated event")
    };
    app.apply(AppEvent::Runtime(RuntimeEvent::Updated(id)));
    app.apply(AppEvent::Runtime(RuntimeEvent::Updated(id)));

    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert_eq!(detail.comments.len(), 1);
}

// -- route_update: issues -------------------------------------------------

#[test]
fn route_update_issues_refreshes_the_focused_base() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);
    assert_eq!(app.list_mut().issues.len(), 1);

    app.runtime
        .create_issue(&IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        })
        .unwrap();
    drain_events(&mut app);

    assert_eq!(app.list_mut().issues.len(), 2);
}

#[test]
fn route_update_issues_under_an_overlay_defers_and_resume_focus_replays() {
    // An overlay above the base: the base's `focused` guard defers the
    // update -- the subscription's slot holds the latest for focus return.
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    fetch_base_list(&mut app, true);
    open_detail_for(&mut app, &issue);

    app.runtime
        .create_issue(&IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        })
        .unwrap();
    drain_events(&mut app);

    // The base is stale -- it never re-read while unfocused.
    assert_eq!(app.list_mut().issues.len(), 1);

    // Popping the overlay replays the deferred update.
    app.pop_view();
    assert_eq!(app.list_mut().issues.len(), 2);
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
        sub: None,
    }));

    app.dispatch_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // The write goes through the runtime, which propagates rather than
    // routing directly; drain it, as `run_app` would in the same frame.
    drain_events(&mut app);

    // The popup pops...
    assert_eq!(app.views.len(), 1);
    // ...and the queued update re-reads the overlay-merged state from the DB.
    assert_eq!(app.list_mut().issues[0].state.name, "Done");
}

#[test]
fn submit_comment_writes_through_the_db_and_refreshes_the_open_detail() {
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    let mut detail = build_cached_detail(&issue, &app.runtime);
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
    let initial_sort = app.list_mut().query.order.field.clone();
    let next_sort = app.list_mut().query.order.field.next();
    app.list_mut().query.order.field = next_sort;
    let replaced = app.list_mut().query.replace_sort_in_filter();
    app.list_mut().query.filter = replaced;
    app.last_esc_time = Some(Instant::now()); // within the 500ms window

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.list_mut().query.order.field, initial_sort);
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
        sub: None,
    }));
}

#[test]
fn floor_esc_pops_detail_overlay() {
    let issue = db_issue("1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(std::slice::from_ref(&issue)).unwrap();
    open_detail_for(&mut app, &issue);
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
    let modal = test_new_issue_modal(&app.runtime);
    app.views.push(View::NewIssue(modal));
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
    let before = app.list_mut().query.order.direction;
    push_priority_popup(&mut app, priority_popup_items());

    app.dispatch_key(key('d'));

    assert_ne!(app.list_mut().query.order.direction, before);
}

#[test]
fn cascade_bound_key_stops_at_the_overlay() {
    // The new-issue modal consumes every key but Esc (a text/form context);
    // `q` must not reach the base's quit.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let modal = test_new_issue_modal(&app.runtime);
    app.views.push(View::NewIssue(modal));

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
    open_detail_for(&mut app, &issue);

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
    let before = app.list_mut().query.order.direction;
    app.views.push(View::Search(SearchOverlay::new()));

    app.dispatch_key(key('d'));

    assert_eq!(app.list_mut().query.order.direction, before);
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
    let mut detail = build_cached_detail(&issue, &app.runtime);
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
            id: "o1".into(),
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
fn consume_sync_event_done_sets_synced_at_from_the_payload() {
    // `Sync(Done)` carries the runtime's own `synced_at` timestamp and only
    // transitions the `sync` typestate; the viewer identity flows through
    // the header's own `ViewerQuery` subscription instead (`apply_viewer_update`).
    let mut app = app_with_db(&[]).unwrap();
    app.sync = SyncStatus::Syncing;
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-10T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    app.consume_sync_event(SyncEvent::Done(Some(now)));

    match &app.sync {
        SyncStatus::Synced { synced_at } => assert_eq!(*synced_at, Some(now)),
        SyncStatus::Idle | SyncStatus::Syncing | SyncStatus::Failed { .. } => {
            unreachable!("expected Synced")
        }
    }
}

// A live `ViewerQuery` update (`apply_viewer_update` setting `Authenticated`
// from a fresh slot value) needs an upstream refresh no `Runtime` write
// method can synthesize -- like the team-scoped subscriptions above, that
// path is exercised at the `lt-runtime` layer instead
// (`crates/lt-runtime/src/runtime.rs`, `refresh_entry_*`,
// `crates/lt-runtime/src/ops.rs`, `refresh_viewer_persists_and_reports_viewer`).

#[test]
fn apply_viewer_update_leaves_auth_unchanged_without_a_fresh_slot_value() {
    // No propagation has touched `Viewer` since subscribing: `take()` finds
    // nothing new, so a matching update is a no-op -- safe to call
    // unconditionally.
    let mut app = app_with_db(&[]).unwrap();
    app.auth = AuthStatus::Unauthenticated;

    app.apply_viewer_update(app.viewer_sub.key());

    assert!(matches!(app.auth, AuthStatus::Unauthenticated));
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

    app.dispatch_key(key('L'));
    assert!(matches!(app.auth, AuthStatus::Authenticating));

    // A second press while already authenticating is a no-op -- the TUI's
    // own gate.
    app.dispatch_key(key('L'));
    assert!(matches!(app.auth, AuthStatus::Authenticating));
}

#[test]
fn refresh_key_resubscribes_the_base_list_immediately() {
    // `ctrl+r` doesn't gate on `Syncing`: an immediate resubscribe runs
    // before the sync request goes out.
    let (mut app, db) = app_with_db_and_handle(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    fetch_base_list(&mut app, true);
    {
        let conn = db.connect().unwrap();
        lt_runtime::test_util::upsert_issues(&conn, &[db_issue("2", "ENG-2", "Todo", 4)]).unwrap();
    }

    app.dispatch_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

    assert_eq!(app.list_mut().issues.len(), 2);
}

/// Seed `team_id` with two ordered workflow states ("Todo" @1.0, "Done" @2.0).
fn seed_team_states(conn: &lt_runtime::test_util::Connection, team_id: &str) -> Result<()> {
    lt_runtime::test_util::upsert_team_state(
        conn,
        team_id,
        &lt_types::types::WorkflowState {
            id: "s-todo".into(),
            name: "Todo".to_string(),
            position: Some(1.0),
        },
    )?;
    lt_runtime::test_util::upsert_team_state(
        conn,
        team_id,
        &lt_types::types::WorkflowState {
            id: "s-done".into(),
            name: "Done".to_string(),
            position: Some(2.0),
        },
    )?;
    Ok(())
}

#[test]
fn route_update_with_no_matching_view_is_a_noop() {
    let mut app = app_with_db(&[]).unwrap();
    let (sub, _) = app.runtime.subscribe::<TeamsQuery>(());
    app.route_update(sub.key());
    assert_eq!(app.views.len(), 1);
}

#[test]
fn new_issue_team_picker_g_selects_the_last_team() {
    // `G` in the new-issue Team picker moves to the last item -- previously
    // a no-op via `Scroll`'s trait defaults.
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    let mut modal = test_new_issue_modal(&app.runtime);
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
fn new_issue_team_change_drops_the_old_scoped_subscriptions_and_subscribes_new_ones() {
    // Leaving the Team field with a different team selected re-subscribes
    // states/members for the new team (RAII replaces the old hand-diffed
    // `watched_team_id` bookkeeping) and marks `loading`.
    let (mut app, db) = app_with_db_and_handle(&[]).unwrap();
    {
        let conn = db.connect().unwrap();
        seed_team_states(&conn, "t2").unwrap();
    }
    let mut modal = test_new_issue_modal(&app.runtime);
    modal.focused_field = NewIssueField::Title;
    modal.teams = vec![
        PopupItem {
            label: "Eng".to_string(),
            id: Some("t1".to_string()),
        },
        PopupItem {
            label: "Design".to_string(),
            id: Some("t2".to_string()),
        },
    ];
    modal.team_selected = 0;
    modal.loading = false;
    app.views.push(View::NewIssue(modal));

    app.dispatch_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)); // Title -> Team
    let Some(View::NewIssue(modal)) = app.views.last_mut() else {
        unreachable!("new-issue view expected")
    };
    modal.team_selected = 1; // select "Design" (t2)

    app.dispatch_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)); // leaves Team

    let Some(View::NewIssue(modal)) = app.views.last() else {
        unreachable!("new-issue view expected")
    };
    assert!(modal.loading);
    assert_eq!(
        modal
            .states
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>(),
        ["Todo", "Done"]
    );
}

#[test]
fn popup_team_scoped_construction_reads_the_current_states() {
    // "Backlog", not "Todo"/"Done", so the issue's own state back-fill
    // doesn't collide with the seeded positioned states below.
    let issue = db_issue("1", "ENG-1", "Backlog", 5);
    let (mut app, db) = app_with_db_and_handle(&[issue]).unwrap();
    fetch_base_list(&mut app, true);
    {
        let conn = db.connect().unwrap();
        seed_team_states(&conn, "ENG").unwrap();
    }

    app.open_state_popup();

    let Some(View::Popup(popup)) = app.views.last() else {
        unreachable!("popup view expected")
    };
    assert_eq!(
        popup
            .items
            .iter()
            .map(|i| i.label.as_str())
            .collect::<Vec<_>>(),
        // Position order first; "Backlog" (the issue's own state
        // back-fill, no recorded position) sorts last by name.
        ["Todo", "Done", "Backlog"]
    );
}

#[test]
fn poll_search_debounce_is_noop_without_pending_change() {
    let mut app = app_with_db(&[]).unwrap();
    // No overlay -> early return.
    poll_search_debounce(&mut app);
    assert_eq!(app.views.len(), 1);
}
