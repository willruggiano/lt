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
    types::Issue {
        id: id.to_string(),
        identifier: ident.to_string(),
        title: format!("issue {ident}"),
        priority_label: "No priority".to_string(),
        priority: 0,
        state: types::WorkflowState {
            id: state.to_string(),
            name: state.to_string(),
        },
        assignee: None,
        team: types::Team {
            id: "ENG".to_string(),
            name: "Engineering".to_string(),
        },
        description: None,
        labels: types::LabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: format!("2026-01-{day:02}T00:00:00Z"),
        updated_at: format!("2026-01-{day:02}T00:00:00Z"),
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
    app.do_fetch(true);
    assert_eq!(app.issues.len(), 3);
    assert_eq!(app.issues[0].identifier, "ENG-1"); // updated DESC
    assert_eq!(app.table_state.selected(), Some(0));
    assert!(!app.pagination.has_next_page);
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
    app.do_fetch(true);
    assert_eq!(app.issues.len(), 2);
    assert!(app.issues.iter().all(|i| i.state.name == "Todo"));
    // run_query has no pagination.
    assert!(!app.pagination.has_next_page);
    assert!(app.pagination.end_cursor.is_none());
}

#[test]
fn do_fetch_and_select_seeks_identifier() {
    let rows = [
        db_issue("1", "ENG-1", "Todo", 5),
        db_issue("2", "ENG-2", "Todo", 4),
        db_issue("3", "ENG-3", "Todo", 3),
    ];
    let mut app = app_with_db(&rows).unwrap();
    app.do_fetch_and_select(Some("ENG-3".to_string()));
    assert_eq!(app.table_state.selected(), Some(2));
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
    app.do_fetch(true);
    assert_eq!(app.issues[0].identifier, "ENG-1");
    assert!(app.pagination.has_next_page);

    app.next_page();
    assert_eq!(app.pagination.current_cursor.as_deref(), Some("2"));
    assert_eq!(app.issues[0].identifier, "ENG-3");

    app.prev_page();
    assert!(app.pagination.current_cursor.is_none());
    assert_eq!(app.issues[0].identifier, "ENG-1");
}

#[test]
fn prev_page_at_start_is_noop() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.do_fetch(true);
    app.prev_page(); // empty cursor stack -> no-op
    assert_eq!(app.issues.len(), 1);
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
    assert_eq!(app.issues.len(), 2);

    let desc_before = app.args.desc;
    app.toggle_desc();
    assert_ne!(app.args.desc, desc_before);
    assert_eq!(app.issues.len(), 2);
}

// -- populate_relations ---------------------------------------------------

#[test]
fn populate_relations_fills_parent_and_children() {
    let mut parent = db_issue("p1", "ENG-9", "Todo", 9);
    parent.title = "the parent".to_string();
    let mut child = db_issue("c1", "ENG-10", "Done", 8);
    child.parent = Some(lt_types::types::Parent {
        id: "p1".to_string(),
        identifier: "ENG-9".to_string(),
    });
    let app = app_with_db(&[parent, child]).unwrap();

    // The issue whose relations we resolve; populate_relations keys off its id.
    let mut issue: lt_types::types::Issue = db_issue("c1", "ENG-10", "Done", 8);
    let mut detail = build_cached_detail(&issue, Vec::new());

    // Seed the issue under a parent so query_children finds it.
    issue.id = "p1".to_string();
    populate_relations(&app.db, &mut detail, &issue);
    assert_eq!(detail.children.len(), 1);
    assert_eq!(detail.children[0].identifier, "ENG-10");
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
    app.do_fetch(true); // populate the list the loop renders
    drive(&mut app, &[key('j'), key('j'), key('q')]).unwrap();
    assert!(app.quit);
    assert_eq!(app.table_state.selected(), Some(2));
}

#[test]
fn run_app_errs_when_events_exhausted_without_quit() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.do_fetch(true);
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

    handle_normal_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.args.sort, initial_sort);
    assert!(app.last_esc_time.is_none());
}

#[test]
fn first_esc_records_timestamp() {
    let mut app = app_with_db(&[db_issue("1", "ENG-1", "Todo", 5)]).unwrap();
    app.last_esc_time = None;
    handle_normal_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
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
    tx.send(SyncEvent::Done(Some(lt_runtime::sync_port::Viewer {
        id: "u1".to_string(),
        name: "Ada".to_string(),
        org_name: "Acme".to_string(),
        org_url_key: "acme".to_string(),
    })))
    .unwrap();
    app.sync.sync_rx = Some(rx);
    poll_sync_events(&mut app);
    assert_eq!(app.viewer_name.as_deref(), Some("Ada"));
    assert!(!app.sync.syncing);
    assert!(app.sync.next_sync_at.is_some());
    assert_eq!(app.issues.len(), 1); // do_fetch ran against the cache
}

#[test]
fn poll_detail_comment_events_done_updates_detail() {
    let issue: lt_types::types::Issue = db_issue("c1", "ENG-1", "Todo", 5);
    let mut app = app_with_db(&[]).unwrap();
    app.detail = Some(build_cached_detail(&issue, Vec::new()));
    let (tx, rx) = mpsc::channel();
    tx.send(CommentSyncEvent::Done(vec![lt_types::types::Comment {
        body: "fresh".to_string(),
        created_at: "2026-01-06T00:00:00Z".to_string(),
        user: None,
    }]))
    .unwrap();
    app.detail_comment_rx = Some(rx);
    poll_detail_comment_events(&mut app);
    assert_eq!(app.detail.unwrap().comments.nodes.len(), 1);
    assert!(app.detail_comment_rx.is_none());
}

#[test]
fn poll_detail_comment_events_error_clears_receiver() {
    let mut app = app_with_db(&[]).unwrap();
    let (tx, rx) = mpsc::channel();
    tx.send(CommentSyncEvent::Error("nope".to_string()))
        .unwrap();
    app.detail_comment_rx = Some(rx);
    poll_detail_comment_events(&mut app);
    assert!(app.detail_comment_rx.is_none());
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
    app.new_issue_modal = Some(NewIssueModal {
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
    });
    app.poll_modal_events();
    let modal = app.new_issue_modal.unwrap();
    assert_eq!(modal.states.len(), 1);
    assert_eq!(modal.assignees.len(), 1);
    assert!(!modal.loading);
}

#[test]
fn poll_search_debounce_is_noop_without_pending_change() {
    let mut app = app_with_db(&[]).unwrap();
    // No overlay -> early return.
    poll_search_debounce(&mut app);
    assert!(app.search_overlay.is_none());
}
