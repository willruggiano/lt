// Rendering tests: drive `ui::render` into a `TestBackend` and snapshot the
// buffer with `insta`, using `App::for_test` state and the deterministic
// `sim` generator. See [[visual-rendering-tests.md]].

use crossterm::event::KeyModifiers;
use lt_types::types::User;
use lt_types::viewer;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// The seeded `sim` dataset's list issues, which the TUI renders.
fn sim_issues(seed: u64, size: usize) -> Vec<Issue> {
    lt_runtime::sim::generate(seed, size).issues
}

/// Draw one frame at `w`x`h` and return the rendered buffer as text.
fn draw(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::render(f, app)).unwrap();
    term.backend().to_string()
}

/// A stable `Authenticated` fixture for a deterministic header identity.
fn authenticated(name: &str, org: &str) -> AuthStatus {
    AuthStatus::Authenticated {
        viewer: viewer::User {
            id: "viewer-1".into(),
            name: name.to_string(),
            organization: viewer::Organization {
                name: org.to_string(),
                url_key: org.to_lowercase(),
            },
        },
    }
}

/// An `App` seeded with sim issues and a fixed identity for a stable header.
/// Fallible (in-memory SQLite setup); callers -- always `#[test]` fns --
/// unwrap.
fn app_with_issues(seed: u64, size: usize) -> Result<App> {
    let mut app = App::for_test(sim_issues(seed, size))?;
    app.auth = authenticated("Ada Lovelace", "Acme");
    Ok(app)
}

fn item(label: &str, id: Option<&str>) -> PopupItem {
    PopupItem {
        label: label.to_string(),
        id: id.map(ToString::to_string),
    }
}

#[test]
fn list_navigation_clamps_within_bounds() {
    let mut app = app_with_issues(0, 10).unwrap();
    app.viewport_height = 4;
    app.list_mut().table_state.select(Some(0));
    let vh = app.viewport_height;

    app.list_mut().scroll(ScrollMotion::Down, vh);
    assert_eq!(app.list_mut().table_state.selected(), Some(1));
    app.list_mut().scroll(ScrollMotion::Up, vh);
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    app.list_mut().scroll(ScrollMotion::Up, vh); // clamp at top
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    app.list_mut().scroll(ScrollMotion::Bottom, vh);
    assert_eq!(app.list_mut().table_state.selected(), Some(9));
    app.list_mut().scroll(ScrollMotion::Top, vh);
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    app.list_mut().scroll(ScrollMotion::PageDown, vh); // +viewport (4)
    assert_eq!(app.list_mut().table_state.selected(), Some(4));
    app.list_mut().scroll(ScrollMotion::HalfPageUp, vh); // -2
    assert_eq!(app.list_mut().table_state.selected(), Some(2));
    app.list_mut().scroll(ScrollMotion::PageUp, vh); // clamp at top
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
}

#[test]
fn navigation_on_empty_list_is_noop() {
    let mut app = App::for_test(Vec::new()).unwrap();
    app.list_mut().scroll(ScrollMotion::Down, 0);
    app.list_mut().scroll(ScrollMotion::Bottom, 0);
    assert_eq!(app.list_mut().table_state.selected(), None);
}

#[test]
fn apply_fetched_selection_resets_or_clamps() {
    let mut app = app_with_issues(0, 3).unwrap();
    app.list_mut().table_state.select(Some(2));
    app.list_mut().apply_fetched_selection(true); // reset
    assert_eq!(app.list_mut().table_state.selected(), Some(0));

    app.list_mut().table_state.select(Some(2));
    app.list_mut().issues.truncate(1); // selection now out of range
    app.list_mut().apply_fetched_selection(false); // clamp
    assert_eq!(app.list_mut().table_state.selected(), Some(0));

    app.list_mut().issues.clear();
    app.list_mut().apply_fetched_selection(false);
    assert_eq!(app.list_mut().table_state.selected(), None);
}

#[test]
fn detail_scroll_saturates() {
    let issue = sim_issues(0, 1)[0].clone();
    let (tx, _rx) = std::sync::mpsc::channel();
    let runtime = test_runtime(lt_runtime::test_util::Database::memory().unwrap(), tx);
    let mut detail = build_cached_detail(&issue, &runtime);
    detail.scroll(ScrollMotion::Down, 10);
    assert_eq!(detail.scroll, 1);
    detail.scroll(ScrollMotion::Up, 10);
    detail.scroll(ScrollMotion::Up, 10); // saturate at 0
    assert_eq!(detail.scroll, 0);
    detail.scroll(ScrollMotion::Bottom, 10);
    assert_eq!(detail.scroll, u16::MAX);
    detail.scroll(ScrollMotion::Top, 10);
    assert_eq!(detail.scroll, 0);
    detail.scroll(ScrollMotion::HalfPageDown, 10); // +5
    assert_eq!(detail.scroll, 5);
    detail.scroll(ScrollMotion::PageUp, 10); // -10, saturating
    assert_eq!(detail.scroll, 0);
}

#[test]
fn popup_move_clamps_and_cancel_resets_stack() {
    // j/Down and Esc aren't bound in the popup's own table; drive them
    // through the full key-dispatch cascade instead of calling the handler
    // directly.
    let mut app = app_with_issues(0, 1).unwrap();
    let issue_id = app.list_mut().issues[0].id.inner().to_string();
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::Priority,
        issue_id,
        team_id: None,
        items: vec![item("a", None), item("b", None), item("c", None)],
        selected: 0,
        sub: None,
    }));

    app.dispatch_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let Some(View::Popup(popup)) = app.views.get(1) else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 1);

    for _ in 0..4 {
        app.dispatch_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    let Some(View::Popup(popup)) = app.views.get(1) else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 2); // clamp at last

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn close_detail_clears_pane_state() {
    // Esc resolves at the floor, which pops the pane -- except the comment
    // input's narrower Esc (cancel the draft) wins first.
    let mut app = app_with_issues(0, 1).unwrap();
    let issue = app.list_mut().issues[0].clone();
    let mut detail = build_cached_detail(&issue, &app.runtime);
    detail.scroll = 5;
    detail.comment_input = Some("draft".to_string());
    app.views.push(View::Detail(Box::new(detail)));

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let Some(View::Detail(detail)) = app.views.last() else {
        unreachable!("detail view expected")
    };
    assert!(detail.comment_input.is_none());
    assert_eq!(app.views.len(), 2); // the pane itself is still open

    app.dispatch_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn filter_sort_sync_and_replacement() {
    let mut app = app_with_issues(0, 1).unwrap();
    app.list_mut().query.filter = search_query::parse_query_ast("sort:title+");
    app.list_mut().query.sync_sort_from_filter();
    assert!(matches!(
        app.list_mut().query.sort,
        lt_runtime::query::SortField::Title
    ));
    assert!(!app.list_mut().query.desc);

    // replace_sort_in_filter rewrites the sort token, preserving other stems.
    app.list_mut().query.sort = lt_runtime::query::SortField::Updated;
    app.list_mut().query.desc = true;
    app.list_mut().query.filter = search_query::parse_query_ast("state:todo sort:title+");
    let replaced = app.list_mut().query.replace_sort_in_filter();
    let (filter, sort) = search_query::lower_ast(&replaced);
    assert_eq!(sort.map(|(_, d)| d), Some(search_query::SortDir::Desc));
    assert_eq!(filter.state.as_deref(), Some("todo"));
}

#[test]
fn new_issue_field_cycles_both_directions() {
    use NewIssueField::{Assignee, Description, Priority, State, Team, Title};
    assert!(matches!(Title.next(), Team));
    assert!(matches!(Description.next(), Title)); // wraps
    assert!(matches!(State.prev(), Priority));
    assert!(matches!(Title.prev(), Title)); // clamps
    assert!(matches!(Assignee.prev(), State));
}

#[test]
fn priority_label_to_u8_maps_levels() {
    assert_eq!(priority_label_to_u8("Urgent"), 1);
    assert_eq!(priority_label_to_u8("high"), 2);
    assert_eq!(priority_label_to_u8("normal"), 3);
    assert_eq!(priority_label_to_u8("medium"), 3);
    assert_eq!(priority_label_to_u8("low"), 4);
    assert_eq!(priority_label_to_u8("No priority"), 0);
}

#[test]
fn assignee_items_put_me_first_and_skip_viewer() {
    let viewer = viewer::User {
        id: "v".into(),
        name: "Vic".to_string(),
        organization: viewer::Organization {
            name: "Acme".to_string(),
            url_key: "acme".to_string(),
        },
    };
    let members = || {
        vec![
            User {
                id: "v".into(),
                name: "Vic".to_string(),
            },
            User {
                id: "m".into(),
                name: "Mara".to_string(),
            },
        ]
    };
    let with_viewer = build_assignee_items(Some(&viewer), members());
    let labels: Vec<&str> = with_viewer.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Me (Vic)", "Unassigned", "Mara"]);

    let no_viewer = build_assignee_items(None, members());
    let labels: Vec<&str> = no_viewer.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Unassigned", "Vic", "Mara"]);
}

#[test]
fn list_view() {
    let mut app = app_with_issues(0, 12).unwrap();
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn list_view_wide_terminal_grows_title_column() {
    let mut app = app_with_issues(0, 12).unwrap();
    insta::assert_snapshot!(draw(&mut app, 160, 20));
}

#[test]
fn empty_list() {
    let mut app = App::for_test(Vec::new()).unwrap();
    app.auth = authenticated("Ada Lovelace", "Acme");
    insta::assert_snapshot!(draw(&mut app, 80, 10));
}

#[test]
fn detail_overlay() {
    let mut app = app_with_issues(0, 12).unwrap();
    let issue = app.list_mut().issues[0].clone();
    let detail = build_cached_detail(&issue, &app.runtime);
    app.views.push(View::Detail(Box::new(detail)));
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn detail_overlay_shows_parent_reference() {
    let mut app = app_with_issues(0, 12).unwrap();
    let mut issue = app.list_mut().issues[0].clone();
    issue.parent = Some(lt_types::types::Parent {
        id: "parent-1".into(),
        identifier: "ENG-1".to_string(),
    });
    let detail = build_cached_detail(&issue, &app.runtime);
    app.views.push(View::Detail(Box::new(detail)));
    let out = draw(&mut app, 100, 24);
    assert!(
        out.contains("Parent: ENG-1"),
        "expected parent reference line, got:\n{out}"
    );
}

#[test]
fn priority_popup() {
    let mut app = app_with_issues(0, 12).unwrap();
    let issue_id = app.list_mut().issues[0].id.inner().to_string();
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::Priority,
        issue_id,
        team_id: None,
        items: priority_popup_items(),
        selected: 1,
        sub: None,
    }));
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn search_overlay() {
    let mut app = app_with_issues(0, 12).unwrap();
    let mut overlay = SearchOverlay::new();
    overlay.results = sim_issues(0, 12);
    overlay.has_searched = true;
    overlay.table_state.select(Some(0));
    app.views.push(View::Search(overlay));
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

/// The pending-chord indicator is the status row's highest-priority
/// branch, reachable from both the list top and a focused Detail view.
#[test]
fn pending_chord_indicator_shows_at_list_top() {
    let mut app = app_with_issues(0, 3).unwrap();
    app.dispatch_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    assert!(app.pending_key.is_some());
    let out = draw(&mut app, 80, 10);
    assert!(
        out.contains("g …"),
        "expected pending-chord indicator, got:\n{out}"
    );
}

#[test]
fn pending_chord_indicator_shows_over_detail_view() {
    let mut app = app_with_issues(0, 3).unwrap();
    let issue = app.list_mut().issues[0].clone();
    let detail = build_cached_detail(&issue, &app.runtime);
    app.views.push(View::Detail(Box::new(detail)));
    app.dispatch_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    assert!(app.pending_key.is_some());
    let out = draw(&mut app, 80, 10);
    assert!(
        out.contains("g …"),
        "expected pending-chord indicator over Detail, got:\n{out}"
    );
}

#[test]
fn help_popup() {
    let mut app = app_with_issues(0, 12).unwrap();
    app.views.push(View::Help(HelpPopup::new()));
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn new_issue_modal() {
    let mut app = app_with_issues(0, 12).unwrap();
    let mut modal = test_new_issue_modal(&app.runtime);
    modal.focused_field = NewIssueField::Title;
    modal.title = TextInput::from("Fix the renderer".to_string());
    modal.description = "Some description.".to_string();
    modal.teams = vec![PopupItem {
        label: "Engineering".to_string(),
        id: Some("ENG".to_string()),
    }];
    modal.team_selected = 0;
    modal.priorities = priority_popup_items();
    modal.states = vec![PopupItem {
        label: "Todo".to_string(),
        id: Some("s1".to_string()),
    }];
    modal.assignees = vec![PopupItem {
        label: "Ada Lovelace".to_string(),
        id: Some("u1".to_string()),
    }];
    modal.loading = false;
    app.views.push(View::NewIssue(modal));
    insta::assert_snapshot!(draw(&mut app, 100, 30));
}
