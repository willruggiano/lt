//! Deterministic dataset generation for simulation testing (feature = "sim").
//!
//! Design: `docs/design/dst.md`.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use clap::Args;
use fake::Fake;
use fake::faker::company::en::{BsNoun, BsVerb, Buzzword, Industry};
use fake::faker::lorem::en::{Paragraph, Sentence, Word};
use fake::faker::name::en::Name;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::db;
use crate::linear::types;

/// Linear's fixed priority vocabulary (matches the labels the TUI renders).
const PRIORITIES: &[&str] = &["No priority", "Urgent", "High", "Normal", "Low"];

/// Standard Linear workflow states.
const STATES: &[&str] = &[
    "Backlog",
    "Todo",
    "In Progress",
    "In Review",
    "Done",
    "Canceled",
];

/// 2026-01-01T00:00:00Z. Fixed base so timestamps never depend on the wall clock.
const BASE_SECS: i64 = 1_767_225_600;

/// A generated, deterministic dataset ready to upsert into the local DB.
#[derive(PartialEq)]
pub struct Dataset {
    pub issues: Vec<types::Issue>,
    pub comments: Vec<db::Comment>,
}

/// Uppercase the first character of `s`.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Derive a unique 3-letter team key (e.g. `ENG`) from a team name, suffixing
/// a digit on collision.
fn team_key(name: &str, used: &HashSet<String>) -> String {
    let base: String = name
        .chars()
        .filter(char::is_ascii_alphabetic)
        .take(3)
        .collect::<String>()
        .to_uppercase();
    let base = if base.is_empty() {
        "TEAM".to_string()
    } else {
        base
    };
    if !used.contains(&base) {
        return base;
    }
    let mut i = 2;
    loop {
        let cand = format!("{base}{i}");
        if !used.contains(&cand) {
            return cand;
        }
        i += 1;
    }
}

/// Build 3-5 teams with distinct names and keys.
fn build_teams(rng: &mut StdRng) -> Vec<(String, String)> {
    let n = rng.random_range(3..6usize);
    let mut teams: Vec<(String, String)> = Vec::with_capacity(n);
    let mut names: HashSet<String> = HashSet::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut attempts = 0;
    while teams.len() < n && attempts < n * 8 {
        attempts += 1;
        let name: String = Industry().fake_with_rng(rng);
        if !names.insert(name.clone()) {
            continue;
        }
        let key = team_key(&name, &keys);
        keys.insert(key.clone());
        teams.push((name, key));
    }
    teams
}

/// Seeded dataset generator. Holds the RNG, the generated teams, and their
/// per-team identifier counters so `ENG-1`, `ENG-2`, ... stay sequential.
struct Generator {
    rng: StdRng,
    seed: u64,
    teams: Vec<(String, String)>,
    team_counters: Vec<u32>,
    base: DateTime<Utc>,
}

impl Generator {
    fn new(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let teams = build_teams(&mut rng);
        let team_counters = vec![0; teams.len()];
        Self {
            rng,
            seed,
            teams,
            team_counters,
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

    fn name(&mut self) -> String {
        Name().fake_with_rng(&mut self.rng)
    }

    fn title(&mut self) -> String {
        let verb: String = BsVerb().fake_with_rng(&mut self.rng);
        let adj: String = Buzzword().fake_with_rng(&mut self.rng);
        let noun: String = BsNoun().fake_with_rng(&mut self.rng);
        capitalize(&format!("{verb} {adj} {noun}"))
    }

    /// A set of 0-3 distinct word labels. The label id mirrors the name so the
    /// relational upsert dedupes a shared label to one row.
    fn labels(&mut self) -> Vec<types::Label> {
        let n = self.rng.random_range(0..4usize);
        let mut chosen: Vec<String> = Vec::with_capacity(n);
        for _ in 0..n {
            let w: String = Word().fake_with_rng(&mut self.rng);
            if !chosen.contains(&w) {
                chosen.push(w);
            }
        }
        chosen
            .into_iter()
            .map(|name| types::Label {
                id: name.clone(),
                name,
            })
            .collect()
    }

    /// A markdown description (heading + paragraph + list) for ~80% of issues,
    /// exercising the detail-pane renderer. The rest have none.
    fn description(&mut self, title: &str) -> Option<String> {
        if self.rng.random_ratio(1, 5) {
            return None;
        }
        let para: String = Paragraph(2..4).fake_with_rng(&mut self.rng);
        let b1: String = Sentence(4..8).fake_with_rng(&mut self.rng);
        let b2: String = Sentence(4..8).fake_with_rng(&mut self.rng);
        let b3: String = Sentence(4..8).fake_with_rng(&mut self.rng);
        Some(format!("## {title}\n\n{para}\n\n- {b1}\n- {b2}\n- {b3}\n"))
    }

    /// A user name for ~80% of issues; `None` (unassigned) for the rest.
    fn maybe_user(&mut self) -> Option<String> {
        if self.rng.random_ratio(1, 5) {
            None
        } else {
            Some(self.name())
        }
    }

    fn maybe_project(&mut self) -> Option<String> {
        if self.rng.random_ratio(6, 10) {
            let p: String = Buzzword().fake_with_rng(&mut self.rng);
            Some(capitalize(&p))
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
    /// guaranteeing every parent references an existing issue. The team id is
    /// the team key (see [`Generator::issue`]).
    fn maybe_parent(&mut self, team_key: &str, existing: &[types::Issue]) -> Option<types::Parent> {
        if !self.rng.random_ratio(3, 20) {
            return None;
        }
        let candidates: Vec<&types::Issue> =
            existing.iter().filter(|e| e.team.id == team_key).collect();
        if candidates.is_empty() {
            return None;
        }
        let p = self.pick(&candidates);
        Some(types::Parent {
            id: p.id.clone(),
            identifier: p.identifier.clone(),
        })
    }

    /// Wrap an optional name as a fragment user whose id mirrors the name, so
    /// the relational upsert dedupes a person to one `users` row.
    fn user(name: Option<String>) -> Option<types::User> {
        name.map(|name| types::User {
            id: name.clone(),
            name,
        })
    }

    fn issue(&mut self, index: usize, existing: &[types::Issue]) -> types::Issue {
        let team_idx = self.rng.random_range(0..self.teams.len());
        let (team_name, team_key) = self.teams[team_idx].clone();
        self.team_counters[team_idx] += 1;
        let identifier = format!("{team_key}-{}", self.team_counters[team_idx]);
        let (created_at, updated_at) = self.timestamps();
        let title = self.title();
        let description = self.description(&title);
        let assignee = Self::user(self.maybe_user());
        let labels = self.labels();
        let project = self.maybe_project();
        let cycle = self.maybe_cycle();
        let creator = Self::user(Some(self.name()));
        let parent = self.maybe_parent(&team_key, existing);
        let priority_label = (*self.pick(PRIORITIES)).to_string();
        let priority = types::priority_label_to_u8(&priority_label);
        let state_name = (*self.pick(STATES)).to_string();
        types::Issue {
            id: format!("sim-{:016x}-{index}", self.seed),
            identifier,
            title,
            priority,
            // The team id is its key; entity ids mirror names so renamed-to-same
            // values collapse to one row in the relational base.
            state: types::State {
                id: state_name.clone(),
                name: state_name,
            },
            assignee,
            team: types::Team {
                id: team_key,
                name: team_name,
            },
            description,
            labels: types::LabelConnection { nodes: labels },
            project: project.map(|name| types::Project {
                id: name.clone(),
                name,
            }),
            cycle: cycle.map(|name| types::Cycle {
                id: name.clone(),
                name: Some(name),
            }),
            creator,
            parent,
            priority_label,
            created_at,
            updated_at,
        }
    }

    fn comments_for(&mut self, issue: &types::Issue) -> Vec<db::Comment> {
        let n = self.rng.random_range(0..4usize);
        let mut out = Vec::with_capacity(n);
        for c in 0..n {
            let (created_at, updated_at) = self.timestamps();
            let body: String = Sentence(8..18).fake_with_rng(&mut self.rng);
            out.push(db::Comment {
                id: format!("{}-c{c}", issue.id),
                issue_id: issue.id.clone(),
                body,
                author_name: Some(self.name()),
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
/// without attempting a network sync, and records a `viewer_name` (a real
/// assignee from the dataset) so the `--assignee=me` filter resolves.
pub fn run(out: &mut dyn Write, args: &SimArgs) -> Result<()> {
    let dataset = generate(args.seed, args.size);
    let conn = db::open_db(db::db_path()?)?;
    db::upsert_issues(&conn, &dataset.issues)?;
    db::upsert_comments(&conn, &dataset.comments)?;
    db::set_meta(&conn, "last_synced_at", &Utc::now().to_rfc3339())?;
    if let Some(name) = dataset
        .issues
        .iter()
        .find_map(|i| i.assignee.as_ref().map(|u| u.name.clone()))
    {
        db::set_meta(&conn, "viewer_name", &name)?;
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

    #[test]
    fn same_seed_is_deterministic() {
        assert!(generate(42, 64) == generate(42, 64));
    }

    #[test]
    fn different_seed_differs() {
        assert!(generate(1, 64) != generate(2, 64));
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
            if let Some(parent) = &issue.parent {
                assert!(
                    ids.contains(parent.id.as_str()),
                    "dangling parent {}",
                    parent.id
                );
                assert_ne!(issue.id, parent.id, "issue is its own parent");
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
        // sanity: relational base reconstructs the rows.
        let args = crate::issues::IssueArgs {
            limit: 250,
            ..Default::default()
        };
        let queried = db::query_issues(&conn, &args).unwrap();
        assert_eq!(queried.len(), 30);
    }
}
