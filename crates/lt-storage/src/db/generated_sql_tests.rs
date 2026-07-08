//! Successor to `sql.rs`'s `every_registered_statement_prepares_and_matches_its_declared_param_count`
//! gate (docs/design/type-safe-sql-adr.md, "Validator"), for the
//! fragment-generated schema and statements `build.rs` writes to `$OUT_DIR`
//! (not yet wired into the shipping schema): every generated DDL statement
//! applies to a fresh database in order, and every generated CRUD statement
//! then prepares against the resulting schema.

include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));
include!(concat!(env!("OUT_DIR"), "/generated_statements.rs"));

#[test]
fn generated_ddl_applies_and_every_generated_statement_prepares() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

    for ddl in GENERATED_DDL {
        conn.execute_batch(ddl)
            .unwrap_or_else(|e| panic!("failed to apply DDL: {e}\n{ddl}"));
    }

    for sql in GENERATED_STATEMENTS {
        conn.prepare(sql)
            .unwrap_or_else(|e| panic!("failed to prepare statement: {e}\n{sql}"));
    }
}
