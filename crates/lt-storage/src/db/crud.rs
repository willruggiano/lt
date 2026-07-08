//! Generated per-entity CRUD: `Insert`/`Update`/`Delete`/`Select` traits
//! implemented over `build.rs`'s `$OUT_DIR/generated_statements.rs` --
//! upsert/update/delete/select statements derived straight from each
//! entity's cynic fragment (`lt-upstream/src/query/types.rs`). Junction
//! fields (`Issue.labels`) and storage-only columns (`issues.synced_at`) are
//! outside these statements' column set; callers layer them on separately
//! (`crate::db::issues::upsert_issue_tx`).
//!
//! [`Select`]'s row-mapping reconstructs only the columns each generated
//! `SELECT` carries: intrinsic scalars plus *raw* foreign-key ids. For
//! [`Issue`], that means its nested entities (`state`, `team`, ...) come back
//! with a real `id` but an empty `name` -- the flat `issues` table stores no
//! denormalized join data; the fully-hydrated read model lives in
//! [`crate::db::issues::issue_from_row`].

use anyhow::{Context, Result};
use lt_upstream::query::scalars::Priority;
use lt_upstream::query::types::{
    Cycle, Issue, IssueLabelConnection, Parent, Project, Team, User, WorkflowState,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::parse_datetime_column;

include!(concat!(env!("OUT_DIR"), "/generated_statements.rs"));

/// Run a parameterized write statement against a generated `&'static str`
/// statement, attaching `what` to any error -- the generated-statement
/// counterpart of `crate::db::sql::execute`, which only accepts the
/// hand-written statement registry's `Sql` newtype.
fn execute(
    conn: &Connection,
    sql: &'static str,
    params: impl rusqlite::Params,
    what: &str,
) -> Result<()> {
    conn.execute(sql, params)
        .with_context(|| format!("failed to {what}"))?;
    Ok(())
}

/// Upsert `Self`'s row via its generated `UPSERT_*` statement.
pub trait Insert {
    fn insert(&self, conn: &Connection) -> Result<()>;
}

/// Update `Self`'s row (by id) via its generated `UPDATE_*` statement.
pub trait Update {
    fn update(&self, conn: &Connection) -> Result<()>;
}

/// Delete a row by id via the generated `DELETE_*` statement.
pub trait Delete {
    fn delete(conn: &Connection, id: &str) -> Result<()>;
}

/// Select a row by id via the generated `SELECT_*` statement.
pub trait Select: Sized {
    fn select(conn: &Connection, id: &str) -> Result<Option<Self>>;
}

// ---------------------------------------------------------------------------
// WorkflowState
// ---------------------------------------------------------------------------

impl Insert for WorkflowState {
    fn insert(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPSERT_WORKFLOW_STATES,
            params![self.id.inner(), self.name, self.position],
            "upsert workflow state entity row",
        )
    }
}

impl Update for WorkflowState {
    fn update(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPDATE_WORKFLOW_STATES,
            params![self.name, self.position, self.id.inner()],
            "update workflow state entity row",
        )
    }
}

impl Delete for WorkflowState {
    fn delete(conn: &Connection, id: &str) -> Result<()> {
        execute(
            conn,
            DELETE_WORKFLOW_STATES,
            params![id],
            "delete workflow state entity row",
        )
    }
}

impl Select for WorkflowState {
    fn select(conn: &Connection, id: &str) -> Result<Option<Self>> {
        conn.query_row(SELECT_WORKFLOW_STATES, params![id], |row| {
            Ok(WorkflowState {
                id: row.get::<_, String>(0)?.into(),
                name: row.get(1)?,
                position: row.get(2)?,
            })
        })
        .optional()
        .context("failed to select workflow state entity row")
    }
}

// ---------------------------------------------------------------------------
// User / Team / Project: identical `{id, name: String}` generated shape
// ---------------------------------------------------------------------------

/// Generates `Insert`/`Update`/`Delete`/`Select` for a `{id, name: String}`
/// entity ([`User`]/[`Team`]/[`Project`]), whose generated statements share
/// the same two-column shape. A declarative macro rather than a shared
/// generic helper, so the near-identical impls don't trip `cargo dupes`.
macro_rules! named_entity_crud {
    ($ty:ident, $upsert:ident, $update:ident, $delete:ident, $select:ident) => {
        impl Insert for $ty {
            fn insert(&self, conn: &Connection) -> Result<()> {
                execute(
                    conn,
                    $upsert,
                    params![self.id.inner(), self.name],
                    concat!("upsert ", stringify!($ty), " entity row"),
                )
            }
        }

        impl Update for $ty {
            fn update(&self, conn: &Connection) -> Result<()> {
                execute(
                    conn,
                    $update,
                    params![self.name, self.id.inner()],
                    concat!("update ", stringify!($ty), " entity row"),
                )
            }
        }

        impl Delete for $ty {
            fn delete(conn: &Connection, id: &str) -> Result<()> {
                execute(
                    conn,
                    $delete,
                    params![id],
                    concat!("delete ", stringify!($ty), " entity row"),
                )
            }
        }

        impl Select for $ty {
            fn select(conn: &Connection, id: &str) -> Result<Option<Self>> {
                conn.query_row($select, params![id], |row| {
                    Ok(Self {
                        id: row.get::<_, String>(0)?.into(),
                        name: row.get(1)?,
                    })
                })
                .optional()
                .context(concat!("failed to select ", stringify!($ty), " entity row"))
            }
        }
    };
}

named_entity_crud!(User, UPSERT_USERS, UPDATE_USERS, DELETE_USERS, SELECT_USERS);
named_entity_crud!(Team, UPSERT_TEAMS, UPDATE_TEAMS, DELETE_TEAMS, SELECT_TEAMS);
named_entity_crud!(
    Project,
    UPSERT_PROJECTS,
    UPDATE_PROJECTS,
    DELETE_PROJECTS,
    SELECT_PROJECTS
);

// ---------------------------------------------------------------------------
// Cycle: `{id, name: Option<String>}`
// ---------------------------------------------------------------------------

impl Insert for Cycle {
    fn insert(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPSERT_CYCLES,
            params![self.id.inner(), self.name],
            "upsert cycle entity row",
        )
    }
}

impl Update for Cycle {
    fn update(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPDATE_CYCLES,
            params![self.name, self.id.inner()],
            "update cycle entity row",
        )
    }
}

impl Delete for Cycle {
    fn delete(conn: &Connection, id: &str) -> Result<()> {
        execute(conn, DELETE_CYCLES, params![id], "delete cycle entity row")
    }
}

impl Select for Cycle {
    fn select(conn: &Connection, id: &str) -> Result<Option<Self>> {
        conn.query_row(SELECT_CYCLES, params![id], |row| {
            Ok(Cycle {
                id: row.get::<_, String>(0)?.into(),
                name: row.get(1)?,
            })
        })
        .optional()
        .context("failed to select cycle entity row")
    }
}

// ---------------------------------------------------------------------------
// Issue
// ---------------------------------------------------------------------------

impl Insert for Issue {
    fn insert(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPSERT_ISSUES,
            params![
                self.id.inner(),
                self.identifier,
                self.title,
                self.priority_label,
                self.priority.0,
                self.state.id.inner(),
                self.assignee.as_ref().map(|u| u.id.inner()),
                self.team.id.inner(),
                self.description,
                self.project.as_ref().map(|p| p.id.inner()),
                self.cycle.as_ref().map(|c| c.id.inner()),
                self.creator.as_ref().map(|u| u.id.inner()),
                self.parent.as_ref().map(|p| p.id.inner()),
                self.created_at.to_rfc3339_millis(),
                self.updated_at.to_rfc3339_millis(),
            ],
            "upsert issue entity row",
        )
    }
}

impl Update for Issue {
    fn update(&self, conn: &Connection) -> Result<()> {
        execute(
            conn,
            UPDATE_ISSUES,
            params![
                self.identifier,
                self.title,
                self.priority_label,
                self.priority.0,
                self.state.id.inner(),
                self.assignee.as_ref().map(|u| u.id.inner()),
                self.team.id.inner(),
                self.description,
                self.project.as_ref().map(|p| p.id.inner()),
                self.cycle.as_ref().map(|c| c.id.inner()),
                self.creator.as_ref().map(|u| u.id.inner()),
                self.parent.as_ref().map(|p| p.id.inner()),
                self.created_at.to_rfc3339_millis(),
                self.updated_at.to_rfc3339_millis(),
                self.id.inner(),
            ],
            "update issue entity row",
        )
    }
}

impl Delete for Issue {
    fn delete(conn: &Connection, id: &str) -> Result<()> {
        execute(conn, DELETE_ISSUES, params![id], "delete issue entity row")
    }
}

/// Reconstruct an [`Issue`] from a [`SELECT_ISSUES`] row: intrinsic scalars
/// plus raw FK ids, with every nested entity's `name` empty (see the module
/// doc comment).
fn issue_from_flat_row(row: &rusqlite::Row) -> rusqlite::Result<Issue> {
    let priority: i64 = row.get("priority")?;
    let state_id: String = row.get("state_id")?;
    let team_id: String = row.get("team_id")?;
    let assignee_id: Option<String> = row.get("assignee_id")?;
    let project_id: Option<String> = row.get("project_id")?;
    let cycle_id: Option<String> = row.get("cycle_id")?;
    let creator_id: Option<String> = row.get("creator_id")?;
    let parent_id: Option<String> = row.get("parent_id")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;

    Ok(Issue {
        id: row.get::<_, String>("id")?.into(),
        identifier: row.get("identifier")?,
        title: row.get("title")?,
        priority_label: row.get("priority_label")?,
        priority: Priority(u8::try_from(priority).unwrap_or(0)),
        state: WorkflowState {
            id: state_id.into(),
            name: String::new(),
            position: 0.0,
        },
        assignee: assignee_id.map(|id| User {
            id: id.into(),
            name: String::new(),
        }),
        team: Team {
            id: team_id.into(),
            name: String::new(),
        },
        description: row.get("description")?,
        labels: IssueLabelConnection { nodes: Vec::new() },
        project: project_id.map(|id| Project {
            id: id.into(),
            name: String::new(),
        }),
        cycle: cycle_id.map(|id| Cycle {
            id: id.into(),
            name: None,
        }),
        creator: creator_id.map(|id| User {
            id: id.into(),
            name: String::new(),
        }),
        parent: parent_id.map(|id| Parent {
            id: id.into(),
            identifier: String::new(),
        }),
        created_at: parse_datetime_column(&created_at)?,
        updated_at: parse_datetime_column(&updated_at)?,
    })
}

impl Select for Issue {
    fn select(conn: &Connection, id: &str) -> Result<Option<Self>> {
        conn.query_row(SELECT_ISSUES, params![id], issue_from_flat_row)
            .optional()
            .context("failed to select issue entity row")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Storage;

    fn conn() -> Connection {
        crate::db::Memory::new().unwrap().connect().unwrap()
    }

    #[test]
    fn workflow_state_round_trips_through_insert_update_select_delete() {
        let conn = conn();
        let state = WorkflowState {
            id: "s1".into(),
            name: "Todo".to_string(),
            position: 1.0,
        };
        state.insert(&conn).unwrap();
        let got = WorkflowState::select(&conn, "s1").unwrap().unwrap();
        assert_eq!(got.name, "Todo");
        assert_eq!(got.position.to_bits(), 1.0_f64.to_bits());

        let updated = WorkflowState {
            name: "Done".to_string(),
            position: 2.0,
            ..state
        };
        updated.update(&conn).unwrap();
        assert_eq!(
            WorkflowState::select(&conn, "s1").unwrap().unwrap().name,
            "Done"
        );

        WorkflowState::delete(&conn, "s1").unwrap();
        assert!(WorkflowState::select(&conn, "s1").unwrap().is_none());
    }

    #[test]
    fn named_entities_round_trip() {
        let conn = conn();

        let team = Team {
            id: "t1".into(),
            name: "Eng".to_string(),
        };
        team.insert(&conn).unwrap();
        assert_eq!(Team::select(&conn, "t1").unwrap().unwrap().name, "Eng");
        let renamed = Team {
            name: "Engineering".to_string(),
            ..team
        };
        renamed.update(&conn).unwrap();
        assert_eq!(
            Team::select(&conn, "t1").unwrap().unwrap().name,
            "Engineering"
        );
        Team::delete(&conn, "t1").unwrap();
        assert!(Team::select(&conn, "t1").unwrap().is_none());

        let user = User {
            id: "u1".into(),
            name: "Ada".to_string(),
        };
        user.insert(&conn).unwrap();
        assert_eq!(User::select(&conn, "u1").unwrap().unwrap().name, "Ada");

        let project = Project {
            id: "p1".into(),
            name: "Platform".to_string(),
        };
        project.insert(&conn).unwrap();
        assert_eq!(
            Project::select(&conn, "p1").unwrap().unwrap().name,
            "Platform"
        );
    }

    #[test]
    fn cycle_round_trips_with_nullable_name() {
        let conn = conn();
        let cycle = Cycle {
            id: "c1".into(),
            name: None,
        };
        cycle.insert(&conn).unwrap();
        assert_eq!(Cycle::select(&conn, "c1").unwrap().unwrap().name, None);

        let named = Cycle {
            id: "c1".into(),
            name: Some("Cycle 7".to_string()),
        };
        named.update(&conn).unwrap();
        assert_eq!(
            Cycle::select(&conn, "c1").unwrap().unwrap().name,
            Some("Cycle 7".to_string())
        );

        Cycle::delete(&conn, "c1").unwrap();
        assert!(Cycle::select(&conn, "c1").unwrap().is_none());
    }

    #[test]
    fn issue_insert_and_select_round_trip_intrinsic_and_fk_columns() {
        let conn = conn();
        let team = Team {
            id: "ENG".into(),
            name: "Engineering".to_string(),
        };
        team.insert(&conn).unwrap();
        let state = WorkflowState {
            id: "s1".into(),
            name: "Todo".to_string(),
            position: 1.0,
        };
        state.insert(&conn).unwrap();

        let issue = Issue {
            id: "1".into(),
            identifier: "ENG-1".to_string(),
            title: "Wire it up".to_string(),
            priority_label: "High".to_string(),
            priority: Priority(2),
            state: state.clone(),
            assignee: None,
            team: team.clone(),
            description: Some("body".to_string()),
            labels: IssueLabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        };
        issue.insert(&conn).unwrap();

        let got = Issue::select(&conn, "1").unwrap().unwrap();
        assert_eq!(got.identifier, "ENG-1");
        assert_eq!(got.title, "Wire it up");
        assert_eq!(got.priority, Priority(2));
        assert_eq!(got.description.as_deref(), Some("body"));
        assert_eq!(got.state.id, state.id);
        assert_eq!(got.team.id, team.id);
        // The flat select carries no joined name -- see the module doc comment.
        assert_eq!(got.state.name, "");
        assert_eq!(got.team.name, "");

        Issue::delete(&conn, "1").unwrap();
        assert!(Issue::select(&conn, "1").unwrap().is_none());
    }
}
