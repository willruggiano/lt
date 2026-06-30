// Rendering tests (docs/design/visual-rendering-tests.md)
//
// These drive `ui::render` into a ratatui `TestBackend` and snapshot the
// resulting buffer with `insta`. They populate `App` state directly via
// `App::for_test` and skip the DB/thread action methods, so no DB, network, or
// profile global is touched. Data comes from the deterministic `sim` generator,
// so the module is gated on `feature = "sim"`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// The seeded `sim` dataset's list issues, which the TUI renders.
fn sim_issues(seed: u64, size: usize) -> Vec<Issue> {
    crate::sim::generate(seed, size).issues
}

/// Draw one frame at `w`x`h` and return the rendered buffer as text.
fn draw(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ui::render(f, app)).unwrap();
    term.backend().to_string()
}

/// An `App` seeded with sim issues and a fixed identity for a stable header.
fn app_with_issues(seed: u64, size: usize) -> App {
    let mut app = App::for_test(sim_issues(seed, size));
    app.viewer_name = Some("Ada Lovelace".to_string());
    app.org_name = Some("Acme".to_string());
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
    app.table_state.select(Some(0));

    app.move_down();
    assert_eq!(app.table_state.selected(), Some(1));
    app.move_up();
    assert_eq!(app.table_state.selected(), Some(0));
    app.move_up(); // clamp at top
    assert_eq!(app.table_state.selected(), Some(0));
    app.move_bottom();
    assert_eq!(app.table_state.selected(), Some(9));
    app.move_top();
    assert_eq!(app.table_state.selected(), Some(0));
    app.page_down(); // +viewport (4)
    assert_eq!(app.table_state.selected(), Some(4));
    app.half_page_up(); // -2
    assert_eq!(app.table_state.selected(), Some(2));
    app.page_up(); // clamp at top
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn navigation_on_empty_list_is_noop() {
    let mut app = App::for_test(Vec::new());
    app.move_down();
    app.move_bottom();
    assert_eq!(app.table_state.selected(), None);
}

#[test]
fn apply_fetched_selection_resets_or_clamps() {
    let mut app = app_with_issues(0, 3);
    app.table_state.select(Some(2));
    app.apply_fetched_selection(true); // reset
    assert_eq!(app.table_state.selected(), Some(0));

    app.table_state.select(Some(2));
    app.issues.truncate(1); // selection now out of range
    app.apply_fetched_selection(false); // clamp
    assert_eq!(app.table_state.selected(), Some(0));

    app.issues.clear();
    app.apply_fetched_selection(false);
    assert_eq!(app.table_state.selected(), None);
}

#[test]
fn detail_scroll_saturates() {
    let mut app = app_with_issues(0, 1);
    app.viewport_height = 10;
    app.detail_scroll_down();
    assert_eq!(app.detail_scroll, 1);
    app.detail_scroll_up();
    app.detail_scroll_up(); // saturate at 0
    assert_eq!(app.detail_scroll, 0);
    app.detail_scroll_to_bottom();
    assert_eq!(app.detail_scroll, u16::MAX);
    app.detail_scroll_to_top();
    assert_eq!(app.detail_scroll, 0);
    app.detail_scroll_half_page_down(); // +5
    assert_eq!(app.detail_scroll, 5);
    app.detail_scroll_page_up(); // -10, saturating
    assert_eq!(app.detail_scroll, 0);
}

#[test]
fn popup_move_clamps_and_cancel_resets_mode() {
    let mut app = app_with_issues(0, 1);
    app.popup_items = vec![item("a", None), item("b", None), item("c", None)];
    app.popup_selected = 0;
    app.popup_move(1);
    assert_eq!(app.popup_selected, 1);
    app.popup_move(5); // clamp at last
    assert_eq!(app.popup_selected, 2);
    app.popup_move(-10); // clamp at first
    assert_eq!(app.popup_selected, 0);

    app.mode = Mode::Popup(PopupKind::Priority);
    app.popup_anchor = Some(ratatui::layout::Rect::new(0, 0, 1, 1));
    app.popup_cancel();
    assert!(matches!(app.mode, Mode::List));
    assert!(app.popup_anchor.is_none());
}

#[test]
fn close_detail_clears_pane_state() {
    let mut app = app_with_issues(0, 1);
    let issue = app.issues[0].clone();
    app.mode = Mode::Detail;
    app.detail = Some(build_cached_detail(&issue, Vec::new()));
    app.detail_scroll = 5;
    app.comment_input = Some("draft".to_string());
    app.close_detail();
    assert!(matches!(app.mode, Mode::List));
    assert!(app.detail.is_none());
    assert_eq!(app.detail_scroll, 0);
    assert!(app.comment_input.is_none());
}

#[test]
fn filter_sort_sync_and_replacement() {
    let mut app = app_with_issues(0, 1);
    app.active_filter = search_query::parse_query_ast("sort:title+");
    app.sync_args_from_filter();
    assert!(matches!(app.args.sort, crate::issues::SortField::Title));
    assert!(!app.args.desc);

    // replace_sort_in_filter rewrites the sort token, preserving other stems.
    app.args.sort = crate::issues::SortField::Updated;
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
fn db_comment_to_api_conversion() {
    let comment = crate::db::Comment {
        id: "c1".to_string(),
        issue_id: "i1".to_string(),
        body: "hi".to_string(),
        author_name: Some("Alice".to_string()),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        synced_at: String::new(),
    };
    let api = crate::linear::types::Comment::from(comment);
    assert_eq!(api.author(), "Alice");
}

#[test]
fn optimistic_builders_apply_popup_choice() {
    let mut app = app_with_issues(0, 1);
    let issue = app.issues[0].clone();

    let built = build_optimistic_issue(&issue, &PopupKind::Priority, &item("Urgent", Some("1")));
    assert_eq!(built.priority_label, "Urgent");
    assert_eq!(built.priority, 1);
    let unassigned = build_optimistic_issue(&issue, &PopupKind::Assignee, &item("x", None));
    assert!(unassigned.assignee.is_none());

    app.table_state.select(Some(0));
    apply_optimistic_in_memory(&mut app, &PopupKind::Priority, &item("Urgent", Some("1")));
    assert_eq!(app.issues[0].priority_label, "Urgent");
    assert_eq!(app.issues[0].priority, 1);
    apply_optimistic_in_memory(&mut app, &PopupKind::Assignee, &item("none", None));
    assert!(app.issues[0].assignee.is_none());
}

#[test]
fn assignee_items_put_me_first_and_skip_viewer() {
    let viewer = crate::linear::viewer::Viewer {
        id: "v".to_string(),
        name: "Vic".to_string(),
        org_name: "Acme".to_string(),
    };
    let members = || {
        vec![
            Member {
                id: "v".to_string(),
                name: "Vic".to_string(),
            },
            Member {
                id: "m".to_string(),
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
    app.viewer_name = Some("Ada Lovelace".to_string());
    app.org_name = Some("Acme".to_string());
    insta::assert_snapshot!(draw(&mut app, 80, 10));
}

#[test]
fn detail_overlay() {
    let mut app = app_with_issues(0, 12);
    let issue = app.issues[0].clone();
    app.detail = Some(build_cached_detail(&issue, Vec::new()));
    app.mode = Mode::Detail;
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn priority_popup() {
    let mut app = app_with_issues(0, 12);
    app.popup_items = priority_popup_items();
    app.popup_selected = 1;
    app.mode = Mode::Popup(PopupKind::Priority);
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn search_overlay() {
    let mut app = app_with_issues(0, 12);
    let mut overlay = SearchOverlay::new();
    overlay.results = sim_issues(0, 12);
    overlay.has_searched = true;
    overlay.table_state.select(Some(0));
    app.search_overlay = Some(overlay);
    app.mode = Mode::Search;
    insta::assert_snapshot!(draw(&mut app, 100, 20));
}

#[test]
fn help_popup() {
    let mut app = app_with_issues(0, 12);
    app.help_popup = Some(HelpPopup::new());
    app.mode = Mode::Help;
    insta::assert_snapshot!(draw(&mut app, 100, 24));
}

#[test]
fn new_issue_modal() {
    let mut app = app_with_issues(0, 12);
    app.new_issue_modal = Some(NewIssueModal {
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
        modal_rx: None,
    });
    app.mode = Mode::NewIssue;
    insta::assert_snapshot!(draw(&mut app, 100, 30));
}
