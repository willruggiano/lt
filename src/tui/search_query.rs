/// Structured search query parser for the TUI search bar (bd-7qo).
///
/// Grammar
/// -------
/// A query string is a whitespace-separated list of tokens.
/// Each token is either a *stem* or a *free-text word*.
///
/// Stems have the form `<key>:<value>`:
///
///   sort:<field><dir>   -- sort order; dir is '+' (asc) or '-' (desc)
///   assignee:<name>     -- filter by assignee name (or "me")
///   priority:<label>    -- filter by priority (urgent/high/normal/low/none)
///   state:<name>        -- filter by workflow state name
///   team:<name>         -- filter by team name or key
///
/// All remaining tokens are concatenated and used as an FTS5 full-text query
/// against the issues_fts index (identifier + title columns).
///
/// Example
/// -------
///   sort:updated- assignee:me priority:urgent state:todo oauth crash
///
/// Parses as:
///   sort   -> Updated, descending
///   assignee -> "me"
///   priority -> "urgent"
///   state    -> "todo"
///   fts_query -> "oauth* crash*"   (prefix-matched)
///
/// Default
/// -------
/// When the user presses `/`, the search bar is pre-populated with
/// `sort:updated-` so the first thing they see is the most recently
/// updated issues in descending order.

use anyhow::Result;
use rusqlite::Connection;

use crate::db::Issue;
use crate::issues::SortField;

// ---------------------------------------------------------------------------
// ParsedQuery -- result of parsing a raw query string
// ---------------------------------------------------------------------------

/// Direction suffix on a sort stem.
#[derive(Debug, Clone, PartialEq)]
pub enum SortDir {
    /// Ascending ('+' suffix or no suffix).
    Asc,
    /// Descending ('-' suffix).
    Desc,
}

/// A fully parsed search query.
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Sort field, if a `sort:` stem was present.
    pub sort: Option<(SortField, SortDir)>,
    /// Assignee filter value (raw string, "me" is treated specially at query time).
    pub assignee: Option<String>,
    /// Priority filter label (normalised to lowercase).
    pub priority: Option<String>,
    /// State filter (substring match, lowercased).
    pub state: Option<String>,
    /// Team filter (substring match).
    pub team: Option<String>,
    /// Free-text words joined into an FTS5 query.  Empty string means no FTS.
    pub fts_terms: String,
}

impl ParsedQuery {
    /// Return `true` when no filter constraints are set and no FTS terms exist.
    pub fn is_empty(&self) -> bool {
        self.sort.is_none()
            && self.assignee.is_none()
            && self.priority.is_none()
            && self.state.is_none()
            && self.team.is_none()
            && self.fts_terms.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a raw query string typed into the TUI search bar.
///
/// Unknown stems are treated as free-text words so that partial typing
/// (e.g. `sort:`) does not produce hard errors.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut sort: Option<(SortField, SortDir)> = None;
    let mut assignee: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut state: Option<String> = None;
    let mut team: Option<String> = None;
    let mut fts_words: Vec<String> = Vec::new();

    for token in raw.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key.to_lowercase().as_str() {
                "sort" => {
                    if let Some((field, dir)) = parse_sort_value(value) {
                        sort = Some((field, dir));
                        continue;
                    }
                    // Unrecognised sort value -- fall through to fts_words.
                }
                "assignee" if !value.is_empty() => {
                    assignee = Some(value.to_lowercase());
                    continue;
                }
                "priority" if !value.is_empty() => {
                    priority = Some(value.to_lowercase());
                    continue;
                }
                "state" if !value.is_empty() => {
                    state = Some(value.to_lowercase());
                    continue;
                }
                "team" if !value.is_empty() => {
                    team = Some(value.to_string());
                    continue;
                }
                _ => {}
            }
        }
        // Plain word -- add to FTS query with prefix wildcard for incremental matching.
        fts_words.push(format!("{}*", token));
    }

    let fts_terms = fts_words.join(" ");

    ParsedQuery {
        sort,
        assignee,
        priority,
        state,
        team,
        fts_terms,
    }
}

/// Parse the value portion of a `sort:` stem.
///
/// Accepted forms:
///   `updated-`   `updated+`   `updated`
///   `created-`   `created+`   `created`
///   `priority-`  `priority+`  `priority`
///   `title-`     `title+`     `title`
///   `assignee-`  `assignee+`  `assignee`
///   `state-`     `state+`     `state`
///   `team-`      `team+`      `team`
fn parse_sort_value(value: &str) -> Option<(SortField, SortDir)> {
    let (field_str, dir) = if let Some(s) = value.strip_suffix('-') {
        (s, SortDir::Desc)
    } else if let Some(s) = value.strip_suffix('+') {
        (s, SortDir::Asc)
    } else {
        (value, SortDir::Asc)
    };

    let field = match field_str.to_lowercase().as_str() {
        "updated" => SortField::Updated,
        "created" => SortField::Created,
        "priority" => SortField::Priority,
        "title" => SortField::Title,
        "assignee" => SortField::Assignee,
        "state" => SortField::State,
        "team" => SortField::Team,
        _ => return None,
    };

    Some((field, dir))
}

// ---------------------------------------------------------------------------
// Normalise priority label
// ---------------------------------------------------------------------------

/// Normalise a user-supplied priority string to the DB label, or return `None`
/// when the string is not a recognised priority.
fn normalise_priority(s: &str) -> Option<&'static str> {
    match s.to_lowercase().as_str() {
        "none" | "no" | "0" => Some("No priority"),
        "urgent" | "1" => Some("Urgent"),
        "high" | "2" => Some("High"),
        "normal" | "medium" | "3" => Some("Normal"),
        "low" | "4" => Some("Low"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SQL execution
// ---------------------------------------------------------------------------

/// Sort-field to SQLite column name.
fn sort_col(field: &SortField) -> &'static str {
    match field {
        SortField::Updated => "updated_at",
        SortField::Created => "created_at",
        SortField::Priority => "priority_label",
        SortField::Title => "title",
        SortField::Assignee => "assignee_name",
        SortField::State => "state_name",
        SortField::Team => "team_name",
    }
}

/// Execute a `ParsedQuery` against the local SQLite database.
///
/// Returns up to `limit` matching `Issue` rows.
///
/// # Errors
///
/// Returns an error if the SQLite query fails (e.g. FTS index unavailable).
pub fn run_query(conn: &Connection, q: &ParsedQuery, limit: usize) -> Result<Vec<Issue>> {
    // Build WHERE conditions and bind parameters.
    let mut conditions: Vec<String> = Vec::new();
    // We collect params as String values and pass them with a macro workaround
    // below; rusqlite requires heterogeneous param lists via the params! macro
    // or by boxing.  We use Box<dyn rusqlite::types::ToSql> for flexibility.
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // -- assignee --
    if let Some(ref a) = q.assignee {
        if a == "me" {
            // "me" without auth context: match the literal string "me" -- callers
            // that have a viewer name should resolve it before calling run_query.
            conditions.push("LOWER(assignee_name) = 'me'".to_string());
        } else {
            conditions.push("LOWER(COALESCE(assignee_name,'')) LIKE ?".to_string());
            bind.push(Box::new(format!("%{}%", a)));
        }
    }

    // -- priority --
    if let Some(ref p) = q.priority {
        if let Some(label) = normalise_priority(p) {
            conditions.push("priority_label = ?".to_string());
            bind.push(Box::new(label.to_string()));
        }
        // Unknown priority string: skip the filter silently so partial typing
        // does not wipe the result list.
    }

    // -- state --
    if let Some(ref s) = q.state {
        conditions.push("LOWER(state_name) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", s)));
    }

    // -- team --
    if let Some(ref t) = q.team {
        conditions.push("(LOWER(team_name) LIKE ? OR LOWER(COALESCE(team_key,'')) LIKE ?)".to_string());
        let pat = format!("%{}%", t.to_lowercase());
        bind.push(Box::new(pat.clone()));
        bind.push(Box::new(pat));
    }

    // -- ORDER BY --
    let (order_col, order_dir) = match &q.sort {
        Some((field, dir)) => (
            sort_col(field),
            if *dir == SortDir::Desc { "DESC" } else { "ASC" },
        ),
        None => ("updated_at", "DESC"),
    };

    // -- FTS --
    let has_fts = !q.fts_terms.is_empty();

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = if has_fts {
        // Join issues with FTS results, apply additional structured filters.
        format!(
            "SELECT i.id, i.identifier, i.title, i.priority_label, i.state_name,
                    i.assignee_name, i.team_name, i.team_key, i.created_at, i.updated_at,
                    i.synced_at
             FROM issues i
             JOIN issues_fts ON issues_fts.rowid = i.rowid
             WHERE issues_fts MATCH ?{extra_cond}
             ORDER BY {col} {dir}
             LIMIT {limit}",
            extra_cond = if conditions.is_empty() {
                String::new()
            } else {
                format!(" AND {}", conditions.join(" AND "))
            },
            col = order_col,
            dir = order_dir,
            limit = limit,
        )
    } else {
        format!(
            "SELECT id, identifier, title, priority_label, state_name,
                    assignee_name, team_name, team_key, created_at, updated_at, synced_at
             FROM issues
             {where_clause}
             ORDER BY {col} {dir}
             LIMIT {limit}",
            where_clause = where_clause,
            col = order_col,
            dir = order_dir,
            limit = limit,
        )
    };

    // Build the final param list: for FTS queries the FTS term goes first.
    let all_params: Vec<Box<dyn rusqlite::types::ToSql>> = if has_fts {
        let mut v: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(q.fts_terms.clone())];
        v.extend(bind);
        v
    } else {
        bind
    };

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| anyhow::anyhow!("prepare search_query: {}", e))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(Issue {
                id: row.get(0)?,
                identifier: row.get(1)?,
                title: row.get(2)?,
                priority_label: row.get(3)?,
                state_name: row.get(4)?,
                assignee_name: row.get(5)?,
                team_name: row.get(6)?,
                team_key: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                synced_at: row.get(10)?,
            })
        })
        .map_err(|e| anyhow::anyhow!("execute search_query: {}", e))?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.map_err(|e| anyhow::anyhow!("read search_query row: {}", e))?);
    }
    Ok(issues)
}

/// Resolve "me" in a parsed query to the actual viewer name.
///
/// If `viewer_name` is Some and the assignee filter is "me", it is replaced
/// with the actual name so that the SQL LIKE filter works correctly.
pub fn resolve_me(q: &mut ParsedQuery, viewer_name: Option<&str>) {
    if q.assignee.as_deref() == Some("me") {
        q.assignee = viewer_name.map(|n| n.to_lowercase());
    }
}

// ---------------------------------------------------------------------------
// Default query string shown when the user presses /
// ---------------------------------------------------------------------------

/// The default query pre-populated in the search bar when the user presses `/`.
pub const DEFAULT_QUERY: &str = "sort:updated-";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_string() {
        let q = parse_query("");
        assert!(q.is_empty());
        assert!(q.sort.is_none());
        assert!(q.fts_terms.is_empty());
    }

    #[test]
    fn parse_default_query() {
        let q = parse_query(DEFAULT_QUERY);
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDir::Desc);
        assert!(q.fts_terms.is_empty());
    }

    #[test]
    fn parse_sort_asc_plus() {
        let q = parse_query("sort:priority+");
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Priority));
        assert_eq!(dir, SortDir::Asc);
    }

    #[test]
    fn parse_sort_no_suffix_defaults_asc() {
        let q = parse_query("sort:title");
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Title));
        assert_eq!(dir, SortDir::Asc);
    }

    #[test]
    fn parse_assignee_me() {
        let q = parse_query("assignee:me");
        assert_eq!(q.assignee.as_deref(), Some("me"));
    }

    #[test]
    fn parse_priority_urgent() {
        let q = parse_query("priority:urgent");
        assert_eq!(q.priority.as_deref(), Some("urgent"));
    }

    #[test]
    fn parse_state_todo() {
        let q = parse_query("state:todo");
        assert_eq!(q.state.as_deref(), Some("todo"));
    }

    #[test]
    fn parse_fts_words() {
        let q = parse_query("oauth crash");
        assert_eq!(q.fts_terms, "oauth* crash*");
    }

    #[test]
    fn parse_mixed_query() {
        let q = parse_query("sort:updated- assignee:me priority:urgent state:todo oauth crash");
        let (field, dir) = q.sort.clone().unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDir::Desc);
        assert_eq!(q.assignee.as_deref(), Some("me"));
        assert_eq!(q.priority.as_deref(), Some("urgent"));
        assert_eq!(q.state.as_deref(), Some("todo"));
        assert_eq!(q.fts_terms, "oauth* crash*");
    }

    #[test]
    fn parse_unknown_sort_field_goes_to_fts() {
        let q = parse_query("sort:bogus");
        // bogus field -> no sort set, "sort:bogus" goes to fts
        assert!(q.sort.is_none());
        assert_eq!(q.fts_terms, "sort:bogus*");
    }

    #[test]
    fn parse_unknown_stem_goes_to_fts() {
        let q = parse_query("foo:bar baz");
        assert_eq!(q.fts_terms, "foo:bar* baz*");
    }

    #[test]
    fn resolve_me_replaces_with_viewer_name() {
        let mut q = parse_query("assignee:me");
        resolve_me(&mut q, Some("Alice"));
        assert_eq!(q.assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn resolve_me_no_viewer_clears_assignee() {
        let mut q = parse_query("assignee:me");
        resolve_me(&mut q, None);
        assert!(q.assignee.is_none());
    }

    #[test]
    fn normalise_priority_variants() {
        assert_eq!(normalise_priority("urgent"), Some("Urgent"));
        assert_eq!(normalise_priority("1"), Some("Urgent"));
        assert_eq!(normalise_priority("high"), Some("High"));
        assert_eq!(normalise_priority("normal"), Some("Normal"));
        assert_eq!(normalise_priority("low"), Some("Low"));
        assert_eq!(normalise_priority("none"), Some("No priority"));
        assert_eq!(normalise_priority("bogus"), None);
    }
}
