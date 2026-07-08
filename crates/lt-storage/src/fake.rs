//! Deterministic dataset generation and seeding (feature = "fake").
//!
//! Design: `docs/design/dst.md`.

use std::collections::HashSet;

use anyhow::{Result, ensure};
use chrono::{DateTime, Duration, Utc};
use fake::Fake;
use fake::faker::company::en::{BsNoun, BsVerb, Buzzword, Industry};
use fake::faker::lorem::en::{Paragraph, Sentence, Word};
use fake::faker::name::en::Name;
use lt_upstream::query::comments::Comment;
use lt_upstream::query::types;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::db::{self, Storage};

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

/// Fixed RNG seed for the generator: `seed`'s output depends only on
/// `Dataset`'s counts, never on ambient input.
const GENERATOR_SEED: u64 = 42;

/// A dataset spec: how many of each entity [`seed`] should generate and write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dataset {
    pub comments: u64,
    pub issues: u64,
    pub users: u64,
    pub teams: u64,
}

impl Default for Dataset {
    fn default() -> Self {
        Self {
            comments: 1_000_000,
            issues: 100_000,
            users: 100,
            teams: 10,
        }
    }
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

/// Build `n` teams with distinct names and keys.
fn build_teams(rng: &mut StdRng, n: usize) -> Vec<(String, String)> {
    let mut teams: Vec<(String, String)> = Vec::with_capacity(n);
    let mut names: HashSet<String> = HashSet::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut attempts = 0;
    let max_attempts = n.saturating_mul(8).max(8);
    while teams.len() < n && attempts < max_attempts {
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

/// Build `n` users with distinct names; a user's id mirrors its name, so the
/// relational upsert dedupes a shared name to one row.
fn build_users(rng: &mut StdRng, n: usize) -> Vec<types::User> {
    let mut users: Vec<types::User> = Vec::with_capacity(n);
    let mut names: HashSet<String> = HashSet::new();
    let mut attempts = 0;
    let max_attempts = n.saturating_mul(8).max(8);
    while users.len() < n && attempts < max_attempts {
        attempts += 1;
        let name: String = Name().fake_with_rng(rng);
        if !names.insert(name.clone()) {
            continue;
        }
        users.push(types::User {
            id: name.clone().into(),
            name,
        });
    }
    users
}

/// Seeded dataset generator. Holds the RNG, the generated teams and users,
/// and the teams' per-team identifier counters so `ENG-1`, `ENG-2`, ... stay
/// sequential. Every issue's assignee, creator, and every comment's author is
/// drawn from `users`, so the written `users` table holds exactly
/// `Dataset::users` rows.
struct Generator {
    rng: StdRng,
    teams: Vec<(String, String)>,
    team_counters: Vec<u32>,
    users: Vec<types::User>,
    base: DateTime<Utc>,
}

impl Generator {
    fn new(team_count: usize, user_count: usize) -> Self {
        let mut rng = StdRng::seed_from_u64(GENERATOR_SEED);
        let teams = build_teams(&mut rng, team_count);
        let team_counters = vec![0; teams.len()];
        let users = build_users(&mut rng, user_count);
        Self {
            rng,
            teams,
            team_counters,
            users,
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
    fn timestamps(
        &mut self,
    ) -> (
        lt_upstream::query::scalars::DateTime,
        lt_upstream::query::scalars::DateTime,
    ) {
        let created = self.rng.random_range(0..15_552_000i64); // up to 180 days
        let updated = created + self.rng.random_range(0..864_000i64); // up to +10 days
        let c = self.base + Duration::seconds(created);
        let u = self.base + Duration::seconds(updated);
        (
            lt_upstream::query::scalars::DateTime(c),
            lt_upstream::query::scalars::DateTime(u),
        )
    }

    fn title(&mut self) -> String {
        let verb: String = BsVerb().fake_with_rng(&mut self.rng);
        let adj: String = Buzzword().fake_with_rng(&mut self.rng);
        let noun: String = BsNoun().fake_with_rng(&mut self.rng);
        capitalize(&format!("{verb} {adj} {noun}"))
    }

    /// A set of 0-3 distinct word labels. The label id mirrors the name so the
    /// relational upsert dedupes a shared label to one row.
    fn labels(&mut self) -> Vec<types::IssueLabel> {
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
            .map(|name| types::IssueLabel {
                id: name.clone().into(),
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

    /// A uniformly-random user from the generated pool, or `None` if the pool
    /// is empty.
    fn pick_user(&mut self) -> Option<types::User> {
        if self.users.is_empty() {
            return None;
        }
        let i = self.rng.random_range(0..self.users.len());
        Some(self.users[i].clone())
    }

    /// A pooled user for ~80% of issues; `None` (unassigned) for the rest.
    fn maybe_user(&mut self) -> Option<types::User> {
        if self.rng.random_ratio(1, 5) {
            None
        } else {
            self.pick_user()
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
        let candidates: Vec<&types::Issue> = existing
            .iter()
            .filter(|e| e.team.id.inner() == team_key)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let p = self.pick(&candidates);
        Some(types::Parent {
            id: p.id.clone(),
            identifier: p.identifier.clone(),
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
        let assignee = self.maybe_user();
        let labels = self.labels();
        let project = self.maybe_project();
        let cycle = self.maybe_cycle();
        let creator = self.pick_user();
        let parent = self.maybe_parent(&team_key, existing);
        // `PRIORITIES` is ordered by level, so the picked index is the level
        // directly -- no label round trip needed.
        let priority_idx = self.rng.random_range(0..PRIORITIES.len());
        let priority_label = PRIORITIES[priority_idx].to_string();
        let priority = u8::try_from(priority_idx).unwrap_or(0);
        // `STATES` is ordered by workflow stage, so the picked index doubles
        // as a stand-in for Linear's stored `position` -- same idiom as
        // `priority_idx` above.
        let state_idx = self.rng.random_range(0..STATES.len());
        let state_name = STATES[state_idx].to_string();
        types::Issue {
            id: format!("issue-{index:016x}").into(),
            identifier,
            title,
            priority: lt_upstream::query::scalars::Priority(priority),
            // The team id is its key; entity ids mirror names so renamed-to-same
            // values collapse to one row in the relational base.
            state: types::WorkflowState {
                id: state_name.clone().into(),
                name: state_name,
                position: f64::from(u32::try_from(state_idx).unwrap_or(0)),
            },
            assignee,
            team: types::Team {
                id: team_key.into(),
                name: team_name,
            },
            description,
            labels: types::IssueLabelConnection { nodes: labels },
            project: project.map(|name| types::Project {
                id: name.clone().into(),
                name,
            }),
            cycle: cycle.map(|name| types::Cycle {
                id: name.clone().into(),
                name: Some(name),
            }),
            creator,
            parent,
            priority_label,
            created_at,
            updated_at,
        }
    }

    /// A single comment attached to `issue_id`, with `index` as its
    /// dataset-unique suffix.
    fn comment(&mut self, index: u64, issue_id: &str) -> Comment {
        let (created_at, updated_at) = self.timestamps();
        let body: String = Sentence(8..18).fake_with_rng(&mut self.rng);
        Comment {
            id: format!("comment-{index:016x}").into(),
            body,
            created_at,
            updated_at,
            user: self.pick_user(),
            issue_id: Some(issue_id.to_string()),
        }
    }
}

/// Every `(team_id, WorkflowState)` pair a generated dataset's issues
/// reference, deduplicated by `(team_id, state_id)`. Sync owns workflow
/// states in production (issue upserts never write them), and the generator
/// has no sync cycle or per-team states API to seed from offline, so this
/// mirrors [`derive_team_memberships_from_issues`](crate::db::derive_team_memberships_from_issues)'s
/// ADR "Sim compatibility" rationale for the workflow-states invariant
/// instead.
fn derive_workflow_states(issues: &[types::Issue]) -> Vec<(String, types::WorkflowState)> {
    let mut seen = HashSet::new();
    let mut states = Vec::new();
    for issue in issues {
        let key = (
            issue.team.id.inner().to_string(),
            issue.state.id.inner().to_string(),
        );
        if seen.insert(key) {
            states.push((issue.team.id.inner().to_string(), issue.state.clone()));
        }
    }
    states
}

/// Deterministically generate `dataset`'s teams, users, workflow states,
/// issues, and comments, and write them into `storage`.
pub fn seed<S: Storage>(storage: &mut S, dataset: Dataset) -> Result<()> {
    let team_count = usize::try_from(dataset.teams).unwrap_or(usize::MAX);
    let user_count = usize::try_from(dataset.users).unwrap_or(usize::MAX);
    let issue_count = usize::try_from(dataset.issues).unwrap_or(usize::MAX);

    ensure!(
        team_count > 0 || issue_count == 0,
        "cannot generate issues with zero teams"
    );

    let mut generator = Generator::new(team_count, user_count);

    let mut issues = Vec::with_capacity(issue_count);
    for index in 0..issue_count {
        let issue = generator.issue(index, &issues);
        issues.push(issue);
    }
    let states = derive_workflow_states(&issues);

    let teams: Vec<types::Team> = generator
        .teams
        .iter()
        .map(|(name, key)| types::Team {
            id: key.clone().into(),
            name: name.clone(),
        })
        .collect();

    let conn = storage.connect()?;
    db::upsert_teams(&conn, &teams)?;
    db::upsert_users(&conn, &generator.users)?;
    for (team_id, state) in &states {
        db::upsert_team_state(&conn, team_id, state)?;
    }
    db::upsert_issues(&conn, &issues)?;

    if !issues.is_empty() {
        let mut comments = Vec::with_capacity(usize::try_from(dataset.comments).unwrap_or(0));
        for index in 0..dataset.comments {
            let issue = &issues[generator.rng.random_range(0..issues.len())];
            comments.push(generator.comment(index, issue.id.inner()));
        }
        db::upsert_comments(&conn, &comments)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::db::{Memory, Select};

    fn small_dataset() -> Dataset {
        Dataset {
            comments: 20,
            issues: 10,
            users: 5,
            teams: 2,
        }
    }

    fn query_all_issues(conn: &rusqlite::Connection) -> Vec<types::Issue> {
        db::query_issues(
            conn,
            &lt_upstream::query::issues::IssuesVariables {
                filter: None,
                sort: None,
                first: Some(250),
                after: None,
            },
        )
        .unwrap()
        .nodes
    }

    #[test]
    fn default_dataset_matches_documented_sizes() {
        let d = Dataset::default();
        assert_eq!(d.comments, 1_000_000);
        assert_eq!(d.issues, 100_000);
        assert_eq!(d.users, 100);
        assert_eq!(d.teams, 10);
    }

    #[test]
    fn fake_seed_round_trips_through_storage() {
        let mut storage = Memory::new().unwrap();
        seed(&mut storage, small_dataset()).unwrap();
        let conn = storage.connect().unwrap();

        // `db::query_teams` (not a raw row count) excludes the sentinel
        // skeleton team `mint_issue_skeleton` mints as an FK anchor for
        // comments and parent references.
        assert_eq!(db::query_teams(&conn).unwrap().len(), 2);
        let user_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_count, 5);

        let issues = query_all_issues(&conn);
        assert_eq!(issues.len(), 10);

        let total_comments: usize = issues
            .iter()
            .map(|i| db::query_comments(&conn, i.id.inner()).unwrap().len())
            .sum();
        assert_eq!(total_comments, 20);

        let via_crud = types::Issue::select(&conn, issues[0].id.inner())
            .unwrap()
            .unwrap();
        assert_eq!(via_crud.identifier, issues[0].identifier);
    }

    #[test]
    fn identifiers_are_unique() {
        let mut storage = Memory::new().unwrap();
        seed(&mut storage, small_dataset()).unwrap();
        let conn = storage.connect().unwrap();

        let issues = query_all_issues(&conn);
        let ids: HashSet<&str> = issues.iter().map(|i| i.id.inner()).collect();
        assert_eq!(ids.len(), issues.len());
        let idents: HashSet<&str> = issues.iter().map(|i| i.identifier.as_str()).collect();
        assert_eq!(idents.len(), issues.len());
    }

    #[test]
    fn parents_reference_generated_issues() {
        let mut storage = Memory::new().unwrap();
        seed(&mut storage, small_dataset()).unwrap();
        let conn = storage.connect().unwrap();

        let issues = query_all_issues(&conn);
        let identifiers: HashSet<&str> = issues.iter().map(|i| i.identifier.as_str()).collect();
        for issue in &issues {
            if let Some(parent) = &issue.parent {
                assert!(
                    identifiers.contains(parent.identifier.as_str()),
                    "dangling parent {}",
                    parent.identifier
                );
                assert_ne!(
                    issue.identifier, parent.identifier,
                    "issue is its own parent"
                );
            }
        }
    }
}
