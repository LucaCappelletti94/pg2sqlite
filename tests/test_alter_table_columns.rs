//! `ALTER TABLE ... ADD COLUMN` and `DROP COLUMN` must be translated, not
//! dropped.
//!
//! SQLite has supported `ADD COLUMN` since 3.1.1 and `DROP COLUMN` since
//! 3.35.0, both below the declared 3.46.0 floor. The translator used to discard
//! every `ALTER TABLE` operation except the two RENAME forms, so a migration
//! adding a column produced no output at all: no error, no warning, and a
//! SQLite schema silently missing the column. For a crate whose entry point is
//! `Pg2Sqlite::ups(migrations_dir)` that is the worst possible failure mode.
//!
//! An added column definition must route through the same column translator the
//! `CREATE TABLE` path uses, so PostgreSQL type mapping and the
//! parenthesisation SQLite requires of non-literal defaults both apply.
//!
//! A multi-operation `ALTER TABLE` becomes one SQLite statement per operation,
//! since SQLite permits only one. It used to be discarded whole by an
//! `operations.len() != 1` early return, so `ADD COLUMN a, ADD COLUMN b` lost
//! both columns silently.

mod helpers;

use diesel::{QueryableByName, prelude::*, sql_types::Text};
use pg2sqlite::{
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
    traits::TranslationOptions,
};

mod schema {
    diesel::table! {
        /// The table as it stands after the migration adds three columns.
        t (id) {
            id -> Integer,
            a -> Text,
            s -> Nullable<Text>,
            flag -> Nullable<Integer>,
            created -> Nullable<Text>,
        }
    }
}

use schema::t as docs;

/// One row of `PRAGMA table_info`. A vendor pragma is the only way to observe a
/// column's DECLARED type, which no typed query can express, so this is the
/// documented exception to using the DSL. Diesel maps result columns by name,
/// so the pragma's other columns are simply ignored.
#[derive(QueryableByName, Debug)]
struct ColumnInfo {
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text, column_name = "type")]
    declared_type: String,
}

fn table_info(conn: &mut SqliteConnection) -> Result<Vec<ColumnInfo>, Box<dyn std::error::Error>> {
    Ok(diesel::sql_query("PRAGMA table_info(t)").load(conn)?)
}

/// Translates `pg` and applies every emitted statement. The emitted DDL is the
/// artifact under test, so it is applied as generated text.
fn apply(pg: &str) -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let translated = Pg2Sqlite::default().sql(pg)?.translate(&Pg2SqliteOptions::default())?;
    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &translated {
        diesel::sql_query(statement.to_string()).execute(&mut conn)?;
    }
    Ok(conn)
}

const BASE: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT NOT NULL);";

/// The migration a real project would write: create, then add columns later.
const MIGRATION: &str = "
    CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT NOT NULL);
    ALTER TABLE t ADD COLUMN s TEXT;
    ALTER TABLE t ADD COLUMN flag BOOLEAN;
    ALTER TABLE t ADD COLUMN created TIMESTAMP DEFAULT now();
";

/// Every added column reaches the SQLite schema and is usable through the typed
/// DSL, which is the whole point: a migration stack must not lose a column.
#[test]
fn added_columns_reach_the_schema_and_are_usable() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(MIGRATION)?;

    diesel::insert_into(docs::table)
        .values((docs::id.eq(1), docs::a.eq("base"), docs::s.eq("added"), docs::flag.eq(1)))
        .execute(&mut conn)?;

    let row: (i32, String, Option<String>, Option<i32>) =
        docs::table.select((docs::id, docs::a, docs::s, docs::flag)).first(&mut conn)?;
    assert_eq!(row, (1, "base".to_owned(), Some("added".to_owned()), Some(1)));

    Ok(())
}

/// An added column's PostgreSQL type goes through the same mapping as a column
/// declared in `CREATE TABLE`: `BOOLEAN` becomes `INTEGER`, `TIMESTAMP` becomes
/// `TEXT`. A hand-built `ALTER TABLE` that skipped the column translator would
/// emit the PostgreSQL type verbatim.
#[test]
fn added_column_types_are_mapped() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(MIGRATION)?;
    let info = table_info(&mut conn)?;
    let declared =
        |name: &str| info.iter().find(|c| c.name == name).map(|c| c.declared_type.to_uppercase());

    assert_eq!(declared("s").as_deref(), Some("TEXT"));
    assert_eq!(declared("flag").as_deref(), Some("INTEGER"), "BOOLEAN must map to INTEGER");
    assert_eq!(declared("created").as_deref(), Some("TEXT"), "TIMESTAMP must map to TEXT");

    Ok(())
}

/// A non-literal default must be parenthesised, and successful execution is the
/// proof.
///
/// SQLite's `DEFAULT` accepts only a literal, a signed number, a bare keyword,
/// or a PARENTHESISED expression. Verified directly:
/// `ADD COLUMN c TEXT DEFAULT datetime('now')` is rejected with
/// `near "(": syntax error`, while `DEFAULT (datetime('now'))` is accepted. So
/// if `apply` succeeds and the default populates, the parentheses were emitted.
///
/// Deliberately NOT asserted through `pragma table_info`: it reports
/// `dflt_value` as `datetime('now')` with the outer parentheses stripped, so an
/// assertion on that string would test a pragma reporting detail rather than
/// the emitted contract, and would fail for a correct translation.
#[test]
fn added_column_non_literal_default_is_applied() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(MIGRATION)?;

    // `created` is omitted, so its default must supply the value.
    diesel::insert_into(docs::table)
        .values((docs::id.eq(1), docs::a.eq("base")))
        .execute(&mut conn)?;

    let created: Option<String> = docs::table.select(docs::created).first(&mut conn)?;
    let created = created.expect("the DEFAULT must have populated the column");
    assert!(
        created.len() >= 19 && created.contains('-') && created.contains(':'),
        "expected a datetime from the translated now() default, got {created:?}"
    );

    Ok(())
}

/// `DROP COLUMN` is emitted and removes the column.
#[test]
fn dropped_column_leaves_the_schema() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn =
        apply(&format!("{BASE} ALTER TABLE t ADD COLUMN s TEXT; ALTER TABLE t DROP COLUMN a;"))?;

    let names: Vec<String> = table_info(&mut conn)?.into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["id".to_owned(), "s".to_owned()], "column a must be gone");

    Ok(())
}

/// SQLite rejects `ADD COLUMN` with a PRIMARY KEY constraint unconditionally,
/// so the translator must refuse rather than emit SQL that cannot execute.
#[test]
fn add_column_with_primary_key_is_rejected() {
    let Err(error) = apply(&format!("{BASE} ALTER TABLE t ADD COLUMN k INTEGER PRIMARY KEY;"))
    else {
        panic!("SQLite cannot add a PRIMARY KEY column, translation must refuse");
    };
    let error = error.to_string();
    assert!(
        error.to_lowercase().contains("primary key"),
        "the error must name the offending constraint, got: {error}"
    );
}

/// Same for a UNIQUE constraint.
#[test]
fn add_column_with_unique_is_rejected() {
    let Err(error) = apply(&format!("{BASE} ALTER TABLE t ADD COLUMN u TEXT UNIQUE;")) else {
        panic!("SQLite cannot add a UNIQUE column, translation must refuse");
    };
    let error = error.to_string();
    assert!(
        error.to_lowercase().contains("unique"),
        "the error must name the offending constraint, got: {error}"
    );
}

/// PostgreSQL allows several operations in one `ALTER TABLE`. SQLite permits
/// one per statement, so the translation fans out. It used to be discarded
/// whole.
#[test]
fn multi_operation_alter_adds_every_column() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT NOT NULL);
        ALTER TABLE t ADD COLUMN s TEXT, ADD COLUMN flag BOOLEAN, ADD COLUMN created TIMESTAMP;
    ",
    )?;

    let names: Vec<String> = table_info(&mut conn)?.into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["id", "a", "s", "flag", "created"].map(String::from).to_vec());

    Ok(())
}

/// Operations of different kinds in one statement all apply, in order.
#[test]
fn multi_operation_alter_mixes_add_and_drop() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = apply(
        "
        CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT NOT NULL);
        ALTER TABLE t ADD COLUMN s TEXT, DROP COLUMN a;
    ",
    )?;

    let names: Vec<String> = table_info(&mut conn)?.into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["id", "s"].map(String::from).to_vec(), "both operations must apply");

    Ok(())
}

/// An operation with no SQLite translation is a hard error rather than a silent
/// drop, per the reporting policy in D2. `ADD CONSTRAINT` cannot be applied to
/// an existing SQLite table without rebuilding it, and losing a CHECK
/// constraint changes which rows the database accepts.
#[test]
fn unsupported_single_operation_is_rejected() {
    let Err(error) = apply(&format!("{BASE} ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0);"))
    else {
        panic!("an untranslatable ALTER TABLE operation must not be silently dropped");
    };
    let error = error.to_string();
    assert!(
        error.to_lowercase().contains("alter table"),
        "the error must name the statement, got: {error}"
    );
}

/// A multi-operation statement containing one untranslatable operation fails
/// whole. Applying the translatable half would leave the database in a state
/// neither PostgreSQL nor the migration author intended.
#[test]
fn unsupported_operation_inside_a_multi_operation_alter_rejects_the_whole_statement() {
    let result =
        apply(&format!("{BASE} ALTER TABLE t ADD COLUMN s TEXT, ADD CONSTRAINT c CHECK (id > 0);"));
    assert!(
        result.is_err(),
        "a partially translatable multi-operation ALTER must not apply its translatable half"
    );
}

/// `ENABLE ROW LEVEL SECURITY` is not untranslatable, it is consumed by the
/// schema and realised as the RLS view and trigger set, so it correctly emits
/// no `ALTER TABLE`. Guards the hard-error rule above against swallowing it:
/// the suite has well over a hundred uses.
#[test]
fn enable_row_level_security_emits_no_alter_and_still_builds_the_wrapper()
-> Result<(), Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let sql = "
        CREATE TABLE docs (id INTEGER PRIMARY KEY, owner_id INTEGER NOT NULL);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON docs FOR SELECT USING (owner_id > 0);
    ";
    let emitted: Vec<String> = Pg2Sqlite::default()
        .sql(sql)?
        .translate(&options)?
        .iter()
        .map(ToString::to_string)
        .collect();

    assert!(
        !emitted.iter().any(|s| s.contains("ALTER TABLE")),
        "RLS enablement must not emit an ALTER TABLE: {emitted:?}"
    );
    assert!(
        emitted.iter().any(|s| s.starts_with("CREATE VIEW docs")),
        "the RLS view must still be built: {emitted:?}"
    );

    Ok(())
}
