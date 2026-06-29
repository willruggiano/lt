//! Deterministic dataset generation for simulation testing (feature = "sim").
//!
//! `lt` is local-first: every read path (issue list, TUI, search, inbox)
//! queries SQLite, and only *populating* the DB ever talks to Linear. `sim`
//! is a second populator that needs no network and no token, driven by a
//! seeded RNG so datasets are reproducible.
//!
//! ```text
//!   Linear GraphQL API ──(token)──> sync ──┐
//!                                           ├─upsert─> SQLite ─query─> list/TUI/search
//!   seed, size ──> generate() ──> Dataset ──┘                         (no token needed)
//! ```
//!
//! `generate` is pure and deterministic -- no wall clock, no thread RNG -- so
//! the same `(seed, size)` always yields byte-identical issues and comments.
//! Knobs: `--seed` and `--size`. Design:
//! `docs/design/deterministic-simulation-testing-adr.md`.

use std::io::Write;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use clap::Args;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::db;

const TEAMS: &[(&str, &str)] = &[
    ("Engineering", "ENG"),
    ("Design", "DES"),
    ("Product", "PRD"),
    ("Operations", "OPS"),
];

const USERS: &[&str] = &[
    "Ada Lovelace",
    "Alan Turing",
    "Grace Hopper",
    "Edsger Dijkstra",
    "Barbara Liskov",
    "Ken Thompson",
    "Margaret Hamilton",
];

const STATES: &[&str] = &[
    "Backlog",
    "Todo",
    "In Progress",
    "In Review",
    "Done",
    "Canceled",
];

const PRIORITIES: &[&str] = &["No priority", "Urgent", "High", "Normal", "Low"];

const LABELS: &[&str] = &[
    "bug",
    "feature",
    "chore",
    "docs",
    "needs-research",
    "needs-design",
    "blocked",
    "good-first-issue",
];

const PROJECTS: &[&str] = &["Core", "Mobile", "Platform", "Growth"];

const VERBS: &[&str] = &[
    "Fix",
    "Add",
    "Refactor",
    "Remove",
    "Document",
    "Investigate",
    "Optimize",
    "Harden",
];

const ADJS: &[&str] = &[
    "flaky",
    "slow",
    "missing",
    "duplicate",
    "stale",
    "unbounded",
    "legacy",
];

const NOUNS: &[&str] = &[
    "sync",
    "cache",
    "parser",
    "renderer",
    "token refresh",
    "pagination",
    "search index",
    "config loader",
];

const COMMENT_BODIES: &[&str] = &[
    "Reproduced locally. Looks like a race in the background thread.",
    "I can pick this up next cycle.",
    "Blocked on the upstream API change.",
    "Added a regression test; ready for review.",
    "Is this still relevant? The code path was removed.",
    "Nice find -- the fix is a one-liner.",
];

/// 2026-01-01T00:00:00Z. Fixed base so timestamps never depend on the wall clock.
const BASE_SECS: i64 = 1_767_225_600;

/// A generated, deterministic dataset ready to upsert into the local DB.
pub struct Dataset {
    pub issues: Vec<db::Issue>,
    pub comments: Vec<db::Comment>,
}

/// Seeded dataset generator. Holds the RNG plus the per-team identifier
/// counters so `ENG-1`, `ENG-2`, ... stay sequential within a team.
struct Generator {
    rng: StdRng,
    seed: u64,
    team_counters: Vec<u32>,
    base: DateTime<Utc>,
}

impl Generator {
    fn new(seed: u64) -> Self {
        Self {
            rng: StdRng::seed_from_u64(seed),
            seed,
            team_counters: vec![0; TEAMS.len()],
            base: DateTime::<Utc>::from_timestamp(BASE_SECS, 0).unwrap_or_default(),
        }
    }

    /// Pick a uniformly-random element of a non-empty slice.
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = self.rng.random_range(0..items.len());
        &items[i]
    }

    /// A `(created_at, updated_at)` pair where `updated_at >= created_at`,
    /// both within ~190 days of the fixed base.
    fn timestamps(&mut self) -> (String, String) {
        let created = self.rng.random_range(0..15_552_000i64); // up to 180 days
        let updated = created + self.rng.random_range(0..864_000i64); // up to +10 days
        let c = self.base + Duration::seconds(created);
        let u = self.base + Duration::seconds(updated);
        (c.to_rfc3339(), u.to_rfc3339())
    }

    fn title(&mut self) -> String {
        format!(
            "{} {} {}",
            self.pick(VERBS),
            self.pick(ADJS),
            self.pick(NOUNS)
        )
    }

    /// A comma-joined set of 0-3 distinct labels (matching the DB column format).
    fn labels(&mut self) -> String {
        let n = self.rng.random_range(0..4usize);
        let mut chosen: Vec<&str> = Vec::with_capacity(n);
        for _ in 0..n {
            let l = *self.pick(LABELS);
            if !chosen.contains(&l) {
                chosen.push(l);
            }
        }
        chosen.join(",")
    }

    /// A markdown description (heading + list) for ~80% of issues, exercising
    /// the detail-pane renderer. The rest have none.
    fn description(&mut self, title: &str) -> Option<String> {
        if self.rng.random_ratio(1, 5) {
            return None;
        }
        Some(format!(
            "## {title}\n\nThe `{}` path needs attention.\n\n- reproduce on `main`\n- add a regression test\n- verify the fix\n",
            self.pick(NOUNS)
        ))
    }

    /// A user name for ~80% of issues; `None` (unassigned) for the rest.
    fn maybe_user(&mut self) -> Option<String> {
        if self.rng.random_ratio(1, 5) {
            None
        } else {
            Some((*self.pick(USERS)).to_string())
        }
    }

    fn maybe<T: AsRef<str>>(&mut self, items: &[T], numerator: u32) -> Option<String> {
        if self.rng.random_ratio(numerator, 10) {
            Some(self.pick(items).as_ref().to_string())
        } else {
            None
        }
    }

    fn maybe_cycle(&mut self) -> Option<String> {
        if self.rng.random_ratio(4, 10) {
            Some(format!("Cycle {}", self.rng.random_range(1..6u32)))
        } else {
            None
        }
    }

    /// Link ~15% of issues to an earlier issue on the same team as a parent,
    /// guaranteeing every `parent_id` references an existing issue.
    fn maybe_parent(
        &mut self,
        team_key: &str,
        existing: &[db::Issue],
    ) -> (Option<String>, Option<String>) {
        if !self.rng.random_ratio(3, 20) {
            return (None, None);
        }
        let candidates: Vec<&db::Issue> = existing
            .iter()
            .filter(|e| e.team_key.as_deref() == Some(team_key))
            .collect();
        if candidates.is_empty() {
            return (None, None);
        }
        let p = self.pick(&candidates);
        (Some(p.id.clone()), Some(p.identifier.clone()))
    }

    fn issue(&mut self, index: usize, existing: &[db::Issue]) -> db::Issue {
        let team_idx = self.rng.random_range(0..TEAMS.len());
        let (team_name, team_key) = TEAMS[team_idx];
        self.team_counters[team_idx] += 1;
        let identifier = format!("{team_key}-{}", self.team_counters[team_idx]);
        let (created_at, updated_at) = self.timestamps();
        let title = self.title();
        let description = self.description(&title);
        let assignee_name = self.maybe_user();
        let labels = self.labels();
        let project_name = self.maybe(PROJECTS, 6);
        let cycle_name = self.maybe_cycle();
        let creator_name = Some((*self.pick(USERS)).to_string());
        let (parent_id, parent_identifier) = self.maybe_parent(team_key, existing);
        db::Issue {
            id: format!("sim-{:016x}-{index}", self.seed),
            identifier,
            title,
            priority_label: (*self.pick(PRIORITIES)).to_string(),
            state_name: (*self.pick(STATES)).to_string(),
            assignee_name,
            team_name: team_name.to_string(),
            team_key: Some(team_key.to_string()),
            created_at,
            updated_at,
            synced_at: String::new(),
            description,
            labels,
            project_name,
            cycle_name,
            creator_name,
            parent_id,
            parent_identifier,
        }
    }

    fn comments_for(&mut self, issue: &db::Issue) -> Vec<db::Comment> {
        let n = self.rng.random_range(0..4usize);
        let mut out = Vec::with_capacity(n);
        for c in 0..n {
            let (created_at, updated_at) = self.timestamps();
            out.push(db::Comment {
                id: format!("{}-c{c}", issue.id),
                issue_id: issue.id.clone(),
                body: (*self.pick(COMMENT_BODIES)).to_string(),
                author_name: Some((*self.pick(USERS)).to_string()),
                created_at,
                updated_at,
                synced_at: String::new(),
            });
        }
        out
    }
}

/// Generate a deterministic dataset of `size` issues (plus their comments).
#[must_use]
pub fn generate(seed: u64, size: usize) -> Dataset {
    let mut generator = Generator::new(seed);
    let mut issues = Vec::with_capacity(size);
    let mut comments = Vec::new();
    for index in 0..size {
        let issue = generator.issue(index, &issues);
        comments.extend(generator.comments_for(&issue));
        issues.push(issue);
    }
    Dataset { issues, comments }
}

/// Knobs for `lt sim`.
#[derive(Args)]
pub struct SimArgs {
    /// RNG seed; the same seed always produces the same dataset.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
    /// Number of issues to generate.
    #[arg(long, default_value_t = 100)]
    pub size: usize,
}

/// Generate a dataset and write it into the active profile's local database.
///
/// Marks the cache fresh so the offline list/TUI serve the generated data
/// without attempting a network sync, and records a `viewer_name` so the
/// `--assignee=me` filter resolves.
pub fn run(out: &mut dyn Write, args: &SimArgs) -> Result<()> {
    let dataset = generate(args.seed, args.size);
    let conn = db::open_db()?;
    db::upsert_issues(&conn, &dataset.issues)?;
    db::upsert_comments(&conn, &dataset.comments)?;
    db::set_meta(&conn, "last_synced_at", &Utc::now().to_rfc3339())?;
    if let Some(first) = USERS.first() {
        db::set_meta(&conn, "viewer_name", first)?;
    }
    writeln!(
        out,
        "Seeded {} issues and {} comments (seed={}, size={}).",
        dataset.issues.len(),
        dataset.comments.len(),
        args.seed,
        args.size
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// Stable fingerprint of a dataset for equality comparison (`db::Issue` has
    /// no `PartialEq`).
    fn fingerprint(d: &Dataset) -> Vec<String> {
        let mut v: Vec<String> = d
            .issues
            .iter()
            .map(|i| {
                format!(
                    "{}|{}|{}|{}|{}|{:?}|{}|{:?}|{}",
                    i.id,
                    i.identifier,
                    i.title,
                    i.priority_label,
                    i.state_name,
                    i.assignee_name,
                    i.labels,
                    i.parent_id,
                    i.created_at
                )
            })
            .collect();
        v.extend(
            d.comments
                .iter()
                .map(|c| format!("{}|{}|{}", c.id, c.issue_id, c.body)),
        );
        v
    }

    #[test]
    fn same_seed_is_deterministic() {
        assert_eq!(
            fingerprint(&generate(42, 64)),
            fingerprint(&generate(42, 64))
        );
    }

    #[test]
    fn different_seed_differs() {
        assert_ne!(fingerprint(&generate(1, 64)), fingerprint(&generate(2, 64)));
    }

    #[test]
    fn size_is_honored() {
        assert_eq!(generate(7, 0).issues.len(), 0);
        assert_eq!(generate(7, 250).issues.len(), 250);
    }

    #[test]
    fn identifiers_are_unique() {
        let d = generate(99, 200);
        let ids: HashSet<&str> = d.issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids.len(), d.issues.len());
        let idents: HashSet<&str> = d.issues.iter().map(|i| i.identifier.as_str()).collect();
        assert_eq!(idents.len(), d.issues.len());
    }

    #[test]
    fn relations_reference_existing_issues() {
        let d = generate(123, 200);
        let ids: HashSet<&str> = d.issues.iter().map(|i| i.id.as_str()).collect();
        for issue in &d.issues {
            if let Some(parent) = &issue.parent_id {
                assert!(ids.contains(parent.as_str()), "dangling parent {parent}");
                assert_ne!(&issue.id, parent, "issue is its own parent");
            }
        }
        for comment in &d.comments {
            assert!(
                ids.contains(comment.issue_id.as_str()),
                "comment {} references missing issue {}",
                comment.id,
                comment.issue_id
            );
        }
    }

    #[test]
    fn round_trips_through_sqlite() {
        let d = generate(5, 30);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        db::upsert_issues(&conn, &d.issues).unwrap();
        db::upsert_comments(&conn, &d.comments).unwrap();
        let args = crate::issues::IssueArgs {
            limit: 250,
            ..Default::default()
        };
        let queried = db::query_issues(&conn, &args).unwrap();
        assert_eq!(queried.len(), 30);
    }
}
