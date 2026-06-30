# Refactor the massive TUI files + align test-org conventions

## Progress (branch claude/scc-codebase-analysis-5w3vgp)

Done, each green under `cargo test` + `cargo test --features sim` + clippy (both
configs); full `make check` passes at the latest commit. `tui/mod.rs`: 4259 ->
905 lines (-79%). Pushed.

- [x] PR1 `docs(testing)` — convention amendment (4217ab2)
- [x] PR2 `render_tests`/`loop_tests` -> sibling files (41bb263)
- [x] PR3 `text_input` module (08b06f4)
- [x] PR4a `convert` module (77eb765)
- [x] PR4b `sync` module (7894dd7)
- [x] PR5 `detail` subsystem (43f9633)
- [x] CI fix: reflow testing.md for prettier proseWrap (7a5bffc)
- [x] Review feedback: strip tracker refs, drop dead `as_str`,
      `format!`->`to_string` (7b27978)
- [x] PR6 `popup` subsystem (ccdfcc3) — PopupKind/PopupItem/HelpPopup/
      SearchOverlay re-exported from `mod.rs` so `ui.rs` was untouched; moved
      open\_\*\_popup/popup\_\*, optimistic helpers, search helpers, and
      handle_popup/help/search_key. Zero pending snapshots.
- [x] PR7 `new_issue` subsystem (bca06f9) — NewIssueField/ModalEvent/
      NewIssueModal re-exported; moved modal lifecycle methods, issue-creation
      free fns (build_create_request/cache_created_issue/build_assignee_items +
      Member/CreatedIssueDisplay), fetch_team_members (re-exported for
      popup.rs), and handle_new_issue_key/handle_description_key. Zero pending
      snapshots.
- [x] PR8 split `ui.rs` -> `ui/` (b33be5d) — 1240 lines -> 10 files
      (mod/util/chrome/table/detail/popup/new_issue/help/search/text_span),
      largest 257. Cross-module helpers are `pub(super)` (ui-internal); only
      `render()` stays `pub`. Render output byte-identical (zero pending
      snapshots). Tracker refs stripped from the moved comments (review #3).
- [x] PR9 idiomatic conversions (review #1/#2/#7) (df0662f) — replaced the free
      `convert.rs` helpers with `From` impls and removed the module.
      `From<db::Issue> for issues::list::Issue` lives in `src/issues/list.rs`
      (already imports `db`; no new edge), carrying `priority_label_to_u8`.
      `From<db::Comment> for linear::types::Comment` lives in
      `src/db/comments.rs` — **not** `linear/types.rs`: placing it on the db
      side keeps `linear::types` a dependency-free leaf and matches the
      consumer→producer arrow (the cache rehydrates API types). Call sites use
      `.map(Into::into)`. `priority_label_to_u8` (lossy) and
      `build_cached_detail` (two inputs) stay functions.
- [x] PR10 `/simplify` test-fixture dedup (3bf18d8) — 4-angle review (reuse,
      simplification, efficiency, altitude) over the authored code. The
      anticipated targets were already clean (jscpd/cargo-dupes 0%;
      `run_query_tests` shares `test_db()` and the multi-stem test already
      loops; render_tests vs loop_tests fixtures are genuinely distinct). One
      real finding: two ~24-line inline `list::Issue` literals in `loop_tests`
      collapsed to `db_issue(..).into()` via PR9's `From`. Other angles: none.

Lessons applied (for resuming):

- `use super::*` in a non-test child module fails `clippy::wildcard_imports`
  (pedantic). Use explicit imports. Common set: `use anyhow::Result;`,
  `use crossterm::event::{KeyCode, KeyModifiers};`, `use std::sync::mpsc;`, plus
  `use super::{App, <enums/types>};`.
- A re-export used only by the sim-gated tests must itself be
  `#[cfg(all(test, feature = "sim"))]` or the default build fails `-D unused`.
- Methods moved into a child submodule become private to it; mark `pub(crate)`
  for callers in `mod.rs`/sibling test modules. Methods that _stay_ in `mod.rs`
  remain reachable from child modules (descendants see ancestor privates).
- Preserve test-module names so insta snapshot paths stay valid.

## Deferred follow-ups (out of this refactor's scope)

Raised in review; tracked here, not done in the extraction PRs:

- **Collapse the DB seam (review #4).** Rename the `DbProvider` trait to
  `Database` and stop modelling on-disk vs in-memory as two trait impls: both
  are SQLite and should differ only by connection path (a file vs `:memory:`).
  Likely one SQLite-backed impl parameterized by path, replacing `RealDb`
  (`src/tui/mod.rs`) and the test `MemoryDb` (`src/tui/loop_tests.rs`).
  Architectural; do as its own change.
- **Clock seam for `build_sync_status_label` (review #6).** It calls
  `chrono::Utc::now()` directly (`src/tui/sync.rs`); per [[testing.md]]
  wall-clock should be threaded as an explicit parameter (cf.
  `relative_age(iso, now_secs)`) for determinism. Pre-existing behavior moved
  verbatim; seam it separately.
- **Align the `From<db::Issue>` placement with `From<db::Comment>`.** PR9 left
  the two rehydration impls on opposite sides: `From<db::Comment>` is db-side
  (`src/db/comments.rs`, `db -> linear`) but `From<db::Issue>` is on the
  destination side (`src/issues/list.rs`, `issues -> db`). The consistent
  end-state is **both** db-side: move `From<db::Issue>` (and
  `priority_label_to_u8`) into `src/db/issues.rs`, inverting to
  `db -> issues::list` like `db/comments`. Precondition: `issues::list`
  currently imports `crate::db` for its query/print helpers, so adding
  `db -> issues::list` today creates an `issues <-> db` cycle. First make
  `issues::list` a leaf (relocate its db-querying code), then invert the arrow.
  Bigger than a move; do as its own change.

## Context

`scc` shows two hand-written source files dwarf everything else (code =
non-blank, non-comment lines):

```
                  file lines   source code   test code   inline test modules
src/tui/mod.rs        4,259        2,549         892      3  (text_input/render/loop)
src/tui/ui.rs         1,240          968           0      0  (its tests live in mod.rs)
```

`mod.rs` is the problem: one file holds the `TextInput` widget, the 84-field
`App` state hub, a 45-method / 877-line `impl App`, ~12 free-function helper
groups, the event loop, 9 key handlers, and 3 test modules
(`src/tui/mod.rs:28-4259`). `ui.rs` is a single 30-function rendering surface
(`src/tui/ui.rs:19-1240`).

Three goals:

1. **Split the source** along its responsibility seams so no file is a grab-bag,
   honoring the strict per-function budgets in `clippy.toml`
   (`too-many-lines-threshold = 80`, `cognitive-complexity-threshold = 20`).
2. **Resolve the test-placement question** ("move mod-level tests to `tests/`?")
   and **update `docs/rules/testing.md`** where research + prior art show our
   rule is stricter than idiomatic Rust warrants.
3. **Learn from the best** — fold proven patterns from mature ratatui apps into
   the target layout, and _explicitly reject_ the ones that don't fit our
   size/design.

Scope (confirmed): `mod.rs` + `ui.rs` only. `search_query.rs` source (~527 LOC)
is cohesive — out of scope; its bulk is tests that stay put. `build.rs`
(codegen) is out of scope. Delivery: incremental stacked PRs. Include a
test-dedup pass.

## Prior art — 4 mature ratatui apps (primary source, cited)

Studied gitu (altsem/gitu, git client), gitui (gitui-org/gitui, git client),
spotify-player (aome510, music), television (alexpasmantier, fuzzy finder).
Sources read from `codeload`/`raw` tarballs (git clone is proxy-blocked).
Citations are repo-relative `path:line`.

**Decomposition consensus (3 of 4 reject a component framework):**

| Pattern                                                                              | Evidence                                                                                                                                                           | Our call                                                                                                                                                                                                                                                                                                    |
| ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One file per pane, free `render_*`/`draw_*(frame, area, &data)` fns, no widget trait | gitu `ui.rs:27`+`screen/`; spotify `ui/page.rs`; television `screen/mod.rs:1-14`,`draw.rs:189`                                                                     | **Adopt** for `ui/`                                                                                                                                                                                                                                                                                         |
| Logic-free `mod.rs` re-export + a small shared-helpers module                        | spotify `ui/utils.rs:11,54,104`; television `screen/mod.rs`                                                                                                        | **Adopt** (`ui/util.rs`)                                                                                                                                                                                                                                                                                    |
| `Component`/`DrawableComponent` trait per widget                                     | only gitui (`components/mod.rs:213-285`, 47 impls)                                                                                                                 | **Reject** — boilerplate tax (trivial popup ≈220 lines, `popups/confirm.rs`); authors flag the two co-implemented traits as a papercut (`components/mod.rs:30-36`). Over-abstraction for our ~5 panes (`posture.md`).                                                                                       |
| Externalize the god-impl's behavior out of the state file                            | gitu `ops/<verb>.rs`+`OpTrait` (`ops/mod.rs:32,115`); television `Action` enum (`action.rs:8`)+central `handle_action` (`television.rs:929`); spotify `command.rs` | **Adopt the goal** (shrink the impl) via `impl super::App` blocks per subsystem — **not** an `Action` enum (next row)                                                                                                                                                                                       |
| Key→`Action`/`Command` enum + centralized dispatch                                   | gitu, gitui, spotify, television all do a version                                                                                                                  | **Reject for now** — all four do it to drive **user-configurable keymaps** (serde). `lt` keybindings are a _static_ `ALL_KEYBINDINGS` array (`src/tui/mod.rs:670`); the indirection is speculative until configurable keys exist. Keep per-mode handler fns; relocate them. Revisit if config keymaps land. |
| Async engine isolated, fed to UI via one channel + drain (not shared `&mut`)         | gitui `asyncgit` + `update_async` (`gitui.rs:95-136`); television `Channel` task; spotify channel-out                                                              | **Already do** — `mpsc` + per-frame `poll_*` (`architecture.md:110-127`). Validated; keep.                                                                                                                                                                                                                  |
| Multi-lock shared state (`Mutex<UIState>`+`RwLock<Data>`)                            | spotify `state/mod.rs:26-40`                                                                                                                                       | **Reject** — that's for a shared-state-in async model; ours is single-threaded UI + message-passing (the pattern gitui/television prefer). Don't regress to shared locks.                                                                                                                                   |
| Separate render thread off an immutable `Ctx` snapshot                               | television `draw.rs:124-189`, `render.rs:60`                                                                                                                       | **Reject** — solves their tokio/async-render contention; our UI is deliberately single-threaded. Out of scope.                                                                                                                                                                                              |

**State decomposition (optional, deferred):** gitu pushes view-local
cursor/scroll state out of the god struct into a `Vec<Screen>` (`app.rs:38`,
`screen/mod.rs:32`), keeping `State` at ~16 fields. Our `App` has 84. Grouping
related fields into sub-structs (e.g. a
`Detail { detail, scroll, comment_rx, comment_input }`) is the applicable idea —
but it is more than a move, so it is a **post-refactor optional PR** (below),
not folded into the extraction PRs.

**Testing — the crisp rule (television) + the gold-standard harness (gitu):**

- **Placement is decided by visibility, per-function** (television `tests/` vs
  inline split): private item → inline `#[cfg(test)] mod`; public-type/binary
  behavior → `tests/`. television paid a visibility tax (made `App.action_tx`
  `pub` + re-exported, `app.rs:31-49`, `tests/app.rs:50-150`) to move behavior
  tests to `tests/`.
- **Our tests need private access** (`App` internals, `ui::render` into a
  buffer, private parser fns), so **inline is correct and a top-level `tests/`
  dir stays out** — the same conclusion 3 of 4 apps reach (gitu/gitui/spotify
  have no `tests/` dir; television uses it only for public-API + PTY-binary
  tests).
- **gitu validates our existing style:** keystroke-replay snapshots over
  ratatui's `TestBackend` (250 `.snap`, `src/tests/helpers/ui.rs`,
  `src/tests/log.rs:1-25`) — what our `render_tests`/`loop_tests` already do via
  `draw()` + sim data (`docs/rules/testing.md:29-43`). Its one new idea: keep
  large cross-cutting tests in an **in-crate `src/tests/` tree** (private access
  retained) + inline micro-widget tests.

## The test-organization question — answered

**Do NOT move these tests to a `tests/` directory.** Two independent grounds:

- **Rust semantics.** `tests/` files compile as _separate crates_, reaching only
  the **public API**; inline/in-crate `#[cfg(test)]` can test **private** items
  —
  [Rust Book ch. 11.3](https://doc.rust-lang.org/book/ch11-03-test-organization.html).
  Confirmed in practice by television's visibility tax.
- **Compile time is a non-reason.** The `#[cfg(test)]` gate already excludes
  tests from `cargo build`; file location is irrelevant
  ([ibid.](https://doc.rust-lang.org/book/ch11-03-test-organization.html)). The
  only payoff of relocating tests is readability.

So research and prior art **validate** the rule's two core decisions (inline
default + no `tests/` dir). They invalidate only one over-broad clause (below).

## Convention update: `docs/rules/testing.md`

Current rule (`docs/rules/testing.md:26-28`): "Tests live in the same file …
There is no `tests/` directory and no separate `*_test.rs` files."

**Keep** the inline default and the no-`tests/`-dir ban (cite Book ch. 11.3 +
television's visibility tax as rationale in the doc). **Relax** the blanket "no
separate test files" clause to permit moving a `#[cfg(test)]` module into an
**in-crate** sibling file when its inline tests dominate file size, retaining
private access via one of:

- From a `mod.rs`: `#[cfg(test)] mod foo_tests;` → `foo_tests.rs` (plain
  **child** module; already how the repo declares submodules,
  `src/db/mod.rs:1-3`).
- From a non-`mod.rs` file:
  `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;` — the `#[path]` keeps
  it a child (private access); a bare sibling `mod` sees only `pub` items
  ([Rust Reference: Modules](https://doc.rust-lang.org/reference/items/modules.html)).
- Or, for a growing suite, a gitu-style in-crate `src/<area>/tests/` subtree
  with a shared harness module (precedent: gitu `src/tests/helpers/`).

Record: naming is `*_tests.rs` (plural, preserves the spirit of the old ban);
this is a **readability escape hatch only** (no compile-time effect), so
**inline stays the default**. Also record the visibility rule of thumb
(television): _needs a private item → inline; only public types/binary →
`tests/`_ — and note we currently have no public-API surface that warrants a
`tests/` dir.

## Target module layout

`src/tui/` already uses `mod.rs` + sibling submodules (`src/tui/mod.rs:1-3`).
Extend it.

```
src/tui/
  mod.rs          wiring + run()/run_app() loop + EventSource/DbProvider DI + key dispatch
  text_input.rs   TextInput + #[cfg(test)] mod text_input_tests (inline; small)
  app.rs          App, Pagination, SyncState, Session; new()/for_test(); list nav + fetch/paginate
  detail.rs       open/close/scroll/submit_comment + poll_detail_comment_events + build_cached_detail/populate_relations + handle_detail_key/handle_comment_input_key
  popup.rs        PopupKind/PopupItem/HelpPopup/SearchOverlay + popup methods + optimistic helpers + search helpers + handle_popup_key/handle_help_key/handle_search_key
  new_issue.rs    NewIssueField/Modal/ModalEvent + new-issue methods + creation free fns + handle_new_issue_key/handle_description_key
  sync.rs         Status/SyncEvent/CommentSyncEvent/LoginEvent + spawn_*_thread + poll_*_events + build_sync_status_label
  convert.rs      db_issue_to_list_issue / db_comment_to_api / priority_label_to_u8
  render_tests.rs #[cfg(all(test, feature="sim"))] mod render_tests   (moved verbatim)
  loop_tests.rs   #[cfg(all(test, feature="sim"))] mod loop_tests     (moved verbatim)
  ui/
    mod.rs        logic-free re-export + render() entry + layout split + render_overlays/render_status_row
    util.rs       shared helpers: to_u16/pct + render_issue_table + selection-clamp (per spotify ui/utils.rs)
    table.rs      render_table/row_cells/date/sort_col_index/TableSpec
    detail.rs     render_detail_overlay/render_detail/build_detail_lines/render_detail_footer
    popup.rs      Popup/render_popup
    new_issue.rs  render_new_issue_modal + title/description/field_picker + submit_key_label
    help.rs       render_help_popup
    search.rs     render_search_overlay/search_row_cells/SortOrder
    chrome.rs     Identity/render_header*/FooterState/render_footer/render_input
    text_span.rs  append_text_input_spans + error_segments/push_*_spans
```

**`impl App` split mechanism.** Keep `struct App` in `app.rs`; put method groups
in their subsystem files as additional `impl super::App { ... }` blocks. Child
modules can access an ancestor type's private fields, so no visibility changes
are needed. Multiple `impl App` blocks across files are idiomatic Rust. This is
a **move**, not a redesign — bodies are unchanged; only `pub(crate)` markers are
added where a call now crosses a module boundary. (This is our lighter analog to
gitu externalizing behavior into `ops/`, without adopting a dispatch enum we
don't need.)

**Rejected alternatives** (with prior-art reasons, see table above): a
`Component` trait (gitui's boilerplate tax), a key→`Action` enum (no
configurable keymaps yet), multi-lock shared state or a separate render thread
(wrong for our single-threaded, message-passing design). Not doing any of these.

## Delivery: stacked PRs (each leaves `make test` + `make check` green)

1. **`docs(testing): permit in-crate sibling test modules`** — the amendment
   above, citing Book ch. 11.3 + the prior-art rule. Docs-only; lands first.
2. **`refactor(tui): extract render_tests/loop_tests to sibling files`** — move
   the two `#[cfg(all(test, feature="sim"))]` modules
   (`src/tui/mod.rs:3427-4259`) verbatim into `src/tui/render_tests.rs` /
   `loop_tests.rs`, declared from `mod.rs`. **Keep module names** so insta
   snapshot paths (`lt__tui__render_tests__*`, `src/tui/snapshots/`) stay valid.
   Biggest line win, lowest risk (pure move).
3. **`refactor(tui): extract text_input module`** — `TextInput` +
   `text_input_tests` (`src/tui/mod.rs:28-490`) → `text_input.rs`. Tier-1: zero
   `App` coupling.
4. **`refactor(tui): extract sync + convert helpers`** — `sync.rs`,
   `convert.rs`.
5. **`refactor(tui): extract detail subsystem`** — `detail.rs`.
6. **`refactor(tui): extract popup + search subsystem`** — `popup.rs`.
7. **`refactor(tui): extract new_issue subsystem`** — `new_issue.rs`. Leaves
   `app.rs` with state + core nav and `mod.rs` with the loop.
8. **`refactor(tui): split ui.rs into ui/ submodules`** — per the `ui/` tree,
   including the shared `ui/util.rs`. Render tests already isolated (PR 2) so
   they shift as a unit.
9. **`refactor(tui): dedupe test fixtures`** (the `/simplify` pass) — collapse
   repeated setup the agents flagged: `search_query.rs` `run_query_tests`
   per-test `issue()` factory (cloned ~10x) + the 4-stem filter matrix
   (`src/tui/search_query.rs:2176-2300`) → parametric/table-driven; fold
   duplicate `draw`/`app_with_issues` fixtures now split across
   `render_tests.rs`/`loop_tests.rs`. Quality-only; assertions unchanged. Run
   `/simplify` on the diff.

**Optional follow-ups (separate, only if desired):**

- `refactor(tui): group App fields into sub-structs` — gitu-style
  view-local-state grouping (e.g. `Detail`, `Popup`) to shrink the 84-field
  struct. Real churn; defer.
- `refactor(tui): unify poll_* channels into one AppEvent queue` — gitui's
  single `InternalEvent` queue (`queue.rs:86-193`) collapses the 4 scattered
  receivers (`sync_rx`/`detail_comment_rx`/`login_rx`/`modal_rx`) + 4 `poll_*`
  fns into one drain. A genuine simplification, but a behavior-touching
  redesign; defer.

## Critical files

- `src/tui/mod.rs` — source of all extractions (`:28-3416` source, `:3427-4259`
  tests).
- `src/tui/ui.rs` — split into `src/tui/ui/`.
- `docs/rules/testing.md:24-33` — the convention amendment.
- `src/tui/snapshots/*.snap` — must not be orphaned; preserve module names.
- Reuse, don't recreate: DI seams `DbProvider`/`EventSource`
  (`src/tui/mod.rs:508-541`) and fixtures
  `draw`/`sim_issues`/`app_with_issues`/`ScriptedEvents`/`MemoryDb` move with
  their code; do not duplicate them.

## Verification

Per PR, in order:

1. `make test` — `cargo test` then `cargo test --features sim`; both pass. PR
   2/8 exercise the sim-gated render/loop tests.
2. `make check` — fmt, clippy (`pedantic`/`cargo`/complexity, all `deny`),
   `cargo deny`, `cargo machete`, jscpd. The complexity gates confirm no
   extracted fn exceeds budget.
3. `make cov` — line-coverage floor unchanged (moves don't change coverage;
   catches a dropped test).
4. Snapshots: a non-empty `cargo insta pending` after a move means a module path
   regressed — fix the module name rather than accepting. Expect **zero**
   changes in PR 2.
5. After PR 8: `make run` (or `nix run . -- tui`) launches and renders against
   the cache.
6. Before/after `scc -a -w` to confirm `mod.rs`/`ui.rs` dropped below a sane
   per-file ceiling and DRYness did not regress.
