// Rendering tests (docs/design/visual-rendering-tests.md)
//
// These drive `ui::render` into a ratatui `TestBackend` and snapshot the
// resulting buffer with `insta`. They populate `App` state directly via
// `App::for_test` and skip the DB/thread action methods, so no DB, network, or
// profile global is touched. Data comes from the deterministic `sim` generator,
// so the module is gated on `feature = "sim"`.

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
fn app_with_issues(seed: u64, size: usize) -> App {
    let mut app = App::for_test(sim_issues(seed, size));
    app.auth = authenticated("Ada Lovelace", "Acme");
    app
}

fn item(label: &str, id: Option<&str>) -> PopupItem {
    PopupItem {
        label: label.to_string(),
        id: id.map(ToString::to_string),
    }
}

#[test]
fn list_navigation_clamps_within_bounds() {
    let mut app = app_with_issues(0, 10);
    app.viewport_height = 4;
    app.list_mut().table_state.select(Some(0));

    app.list_mut().move_down();
    assert_eq!(app.list_mut().table_state.selected(), Some(1));
    app.list_mut().move_up();
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    app.list_mut().move_up(); // clamp at top
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    app.list_mut().move_bottom();
    assert_eq!(app.list_mut().table_state.selected(), Some(9));
    app.list_mut().move_top();
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
    let vh = app.viewport_height;
    app.list_mut().page_down(vh); // +viewport (4)
    assert_eq!(app.list_mut().table_state.selected(), Some(4));
    app.list_mut().half_page_up(vh); // -2
    assert_eq!(app.list_mut().table_state.selected(), Some(2));
    app.list_mut().page_up(vh); // clamp at top
    assert_eq!(app.list_mut().table_state.selected(), Some(0));
}

#[test]
fn navigation_on_empty_list_is_noop() {
    let mut app = App::for_test(Vec::new());
    app.list_mut().move_down();
    app.list_mut().move_bottom();
    assert_eq!(app.list_mut().table_state.selected(), None);
}

#[test]
fn apply_fetched_selection_resets_or_clamps() {
    let mut app = app_with_issues(0, 3);
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
    let mut detail = build_cached_detail(&issue, Vec::new());
    detail.scroll_down();
    assert_eq!(detail.scroll, 1);
    detail.scroll_up();
    detail.scroll_up(); // saturate at 0
    assert_eq!(detail.scroll, 0);
    detail.scroll_to_bottom();
    assert_eq!(detail.scroll, u16::MAX);
    detail.scroll_to_top();
    assert_eq!(detail.scroll, 0);
    detail.scroll_half_page_down(10); // +5
    assert_eq!(detail.scroll, 5);
    detail.scroll_page_up(10); // -10, saturating
    assert_eq!(detail.scroll, 0);
}

#[test]
fn popup_move_clamps_and_cancel_resets_stack() {
    let mut app = app_with_issues(0, 1);
    let issue_id = app.list_mut().issues[0].id.inner().to_string();
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::Priority,
        issue_id,
        team_id: None,
        items: vec![item("a", None), item("b", None), item("c", None)],
        selected: 0,
        anchor: Some(ratatui::layout::Rect::new(0, 0, 1, 1)),
    }));

    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    let Some(View::Popup(popup)) = app.views.get(1) else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 1);

    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    handle_popup_key(
        &mut app,
        1,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    let Some(View::Popup(popup)) = app.views.get(1) else {
        unreachable!("popup view expected")
    };
    assert_eq!(popup.selected, 2); // clamp at last

    handle_popup_key(&mut app, 1, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.views.len(), 1);
}

#[test]
fn close_detail_clears_pane_state() {
    let mut app = app_with_issues(0, 1);
    let issue = app.list_mut().issues[0].clone();
    let mut detail = build_cached_detail(&issue, Vec::new());
    detail.scroll = 5;
    detail.comment_input = Some("draft".to_string());
    app.views.push(View::Detail(Box::new(detail)));
    app.close_detail();
    assert_eq!(app.views.len(), 1);
}

#[test]
fn filter_sort_sync_and_replacement() {
    let mut app = app_with_issues(0, 1);
    app.active_filter = search_query::parse_query_ast("sort:title+");
    app.sync_args_from_filter();
    assert!(matches!(app.args.sort, lt_runtime::query::SortField::Title));
    assert!(!app.args.desc);

    // replace_sort_in_filter rewrites the sort token, preserving other stems.
    app.args.sort = lt_runtime::query::SortField::Updated;
    app.args.desc = true;
    app.active_filter = search_query::parse_query_ast("state:todo sort:title+");
    let replaced = app.replace_sort_in_filter();
    let parsed = search_query::ParsedQuery::from(&replaced);
    assert_eq!(
        parsed.sort.map(|(_, d)| d),
        Some(search_query::SortDir::Desc)
    );
    assert_eq!(parsed.state.as_deref(), Some("todo"));
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
    // `viewer` is the persisted `db::synced_viewer` shape, not the live
    // API `viewer::User` -- this is the "Me (...)" resolution the modal
    // uses at consume time.
    let viewer = User {
        id: "v".into(),
        name: "Vic".to_string(),
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
    let mut app = app_with_issues(0, 12);
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn empty_list() {
    let mut app = App::for_test(Vec::new());
    app.auth = authenticated("Ada Lovelace", "Acme");
    insta::assert_snapshot!(draw(&mut app, 80, 10));
}

#[test]
fn detail_overlay() {
    let mut app = app_with_issues(0, 12);
    let issue = app.list_mut().issues[0].clone();
    app.views.push(View::Detail(Box::new(build_cached_detail(
        &issue,
        Vec::new(),
    ))));
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn priority_popup() {
    let mut app = app_with_issues(0, 12);
    let issue_id = app.list_mut().issues[0].id.inner().to_string();
    app.views.push(View::Popup(PopupView {
        kind: PopupKind::Priority,
        issue_id,
        team_id: None,
        items: priority_popup_items(),
        selected: 1,
        anchor: None,
    }));
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn search_overlay() {
    let mut app = app_with_issues(0, 12);
    let mut overlay = SearchOverlay::new();
    overlay.results = sim_issues(0, 12);
    overlay.has_searched = true;
    overlay.table_state.select(Some(0));
    app.views.push(View::Search(overlay));
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn help_popup() {
    let mut app = app_with_issues(0, 12);
    app.views.push(View::Help(HelpPopup::new()));
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn new_issue_modal() {
    let mut app = app_with_issues(0, 12);
    app.views.push(View::NewIssue(NewIssueModal {
        focused_field: NewIssueField::Title,
        title: TextInput::from("Fix the renderer".to_string()),
        description: "Some description.".to_string(),
        teams: vec![PopupItem {
            label: "Engineering".to_string(),
            id: Some("ENG".to_string()),
        }],
        team_selected: 0,
        priorities: priority_popup_items(),
        priority_selected: 0,
        states: vec![PopupItem {
            label: "Todo".to_string(),
            id: Some("s1".to_string()),
        }],
        state_selected: 0,
        assignees: vec![PopupItem {
            label: "Ada Lovelace".to_string(),
            id: Some("u1".to_string()),
        }],
        assignee_selected: 0,
        loading: false,
        error: String::new(),
    }));
    insta::assert_snapshot!(draw(&mut app, 100, 30));
}
