/// A key resolves to an `Action`, interpreted by the context that resolved
/// it -- e.g. `MoveDown` is list-selection movement in `List`, offset
/// scrolling in `Detail` (`docs/design/keybinds.md`, Architecture). Phase 1
/// carries only the variants the List/Detail/Popup contexts use; later
/// phases add their own (forms, search, help) as each context lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    // Navigation -- mapped onto `ScrollMotion` and applied through
    // `View::scroll` (Decision 6 of `docs/design/tui-app-event-queue-adr.md`)
    // rather than through a per-context `apply_*` function.
    MoveUp,
    MoveDown,
    MoveTop,
    MoveBottom,
    HalfPageUp,
    HalfPageDown,
    PageUp,
    PageDown,
    NextPage,
    PrevPage,
    // App-level.
    OpenHelp,
    OpenSearch,
    OpenDetail,
    CreateIssue,
    Refresh,
    Login,
    OpenInBrowser,
    // Issue fields.
    SetStatus,
    SetPriority,
    SetAssignee,
    ToggleSortDirection,
    Comment,
    // Forms/popups.
    Confirm,
}

impl Action {
    /// Display name for the help overlay (and a future command palette).
    /// Unused outside tests until phase 3 wires `help_rows()` -- `#[cfg(test)]`
    /// so the plain (non-test) build doesn't flag it dead in the meantime.
    #[cfg(test)]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Action::MoveUp => "move up",
            Action::MoveDown => "move down",
            Action::MoveTop => "go to top",
            Action::MoveBottom => "go to bottom",
            Action::HalfPageUp => "half page up",
            Action::HalfPageDown => "half page down",
            Action::PageUp => "page up",
            Action::PageDown => "page down",
            Action::NextPage => "next page",
            Action::PrevPage => "previous page",
            Action::OpenHelp => "open keyboard shortcuts",
            Action::OpenSearch => "search",
            Action::OpenDetail => "open detail pane",
            Action::CreateIssue => "create issue",
            Action::Refresh => "refresh",
            Action::Login => "log in / re-authenticate",
            Action::OpenInBrowser => "open in browser",
            Action::SetStatus => "set status",
            Action::SetPriority => "set priority",
            Action::SetAssignee => "set assignee",
            Action::ToggleSortDirection => "toggle sort direction",
            Action::Comment => "comment on issue",
            Action::Confirm => "confirm",
        }
    }
}
