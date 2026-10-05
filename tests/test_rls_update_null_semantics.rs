//! An UPDATE through an RLS view must be able to set a nullable column to NULL,
//! and must leave columns absent from the SET clause untouched.
//!
//! The generated `INSTEAD OF UPDATE` trigger used to emit
//! `SET col = COALESCE(NEW.col, OLD.col)` for every column, justified by the
//! claim that SQLite only defines `NEW.column` for columns named in the SET
//! clause. That claim is false: SQLite fully populates `NEW`, and a column
//! absent from the SET clause already carries its OLD value. The COALESCE was
//! therefore unnecessary, and it silently turned `SET col = NULL` into a no-op.
//!
//! `update_absent_column_is_preserved` is the guard proving the COALESCE was
//! not load-bearing. It must pass both before and after its removal.

mod helpers;

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        /// RLS view over `docs_rls`. Writes route through INSTEAD OF triggers.
        docs (id) {
            id -> Integer,
            owner_id -> Integer,
            body -> Nullable<Text>,
            note -> Nullable<Text>,
        }
    }

    diesel::table! {
        /// Backing table. Read directly so assertions observe what was really
        /// stored rather than what the view chooses to show.
        docs_rls (id) {
            id -> Integer,
            owner_id -> Integer,
            body -> Nullable<Text>,
            note -> Nullable<Text>,
        }
    }
}

use schema::{docs, docs_rls};

#[derive(Insertable)]
#[diesel(table_name = docs)]
struct NewDoc {
    id: i32,
    owner_id: i32,
    body: Option<String>,
    note: Option<String>,
}

#[derive(Queryable, Selectable, Debug, PartialEq, Eq)]
#[diesel(table_name = docs_rls)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct StoredDoc {
    id: i32,
    owner_id: i32,
    body: Option<String>,
    note: Option<String>,
}

const PG: &str = "
    CREATE TABLE docs (
        id INTEGER PRIMARY KEY,
        owner_id INTEGER NOT NULL,
        body TEXT,
        note TEXT
    );
    ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
    CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
    CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner_id > 0);
    CREATE POLICY docs_update ON docs FOR UPDATE USING (owner_id > 0) WITH CHECK (owner_id > 0);
";

/// Translates `PG` and applies the result. The emitted DDL is the artifact
/// under test, so it is applied as generated text. Every subsequent statement
/// uses the typed DSL against the schemas above.
fn setup() -> Result<SqliteConnection, Box<dyn std::error::Error>> {
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
    let translated = Pg2Sqlite::default().sql(PG)?.translate(&options)?;

    let mut conn = SqliteConnection::establish(":memory:")?;
    for statement in &translated {
        diesel::sql_query(statement.to_string()).execute(&mut conn)?;
    }

    diesel::insert_into(docs::table)
        .values(NewDoc {
            id: 1,
            owner_id: 7,
            body: Some("original body".to_owned()),
            note: Some("original note".to_owned()),
        })
        .execute(&mut conn)?;

    Ok(conn)
}

fn stored(conn: &mut SqliteConnection) -> Result<StoredDoc, Box<dyn std::error::Error>> {
    Ok(docs_rls::table.filter(docs_rls::id.eq(1)).select(StoredDoc::as_select()).first(conn)?)
}

/// Setting a nullable column to NULL through the view must store NULL.
#[test]
fn update_sets_nullable_column_to_null() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup()?;

    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq(None::<String>))
        .execute(&mut conn)?;

    let row = stored(&mut conn)?;
    assert_eq!(
        row.body, None,
        "SET body = NULL must store NULL, COALESCE(NEW.body, OLD.body) resurrects the old value"
    );
    assert_eq!(
        row.note,
        Some("original note".to_owned()),
        "a column absent from the SET clause must keep its value"
    );

    Ok(())
}

/// A column absent from the SET clause keeps its value. This holds because
/// SQLite populates `NEW` fully, which is why no COALESCE is needed.
#[test]
fn update_absent_column_is_preserved() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup()?;

    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set(docs::body.eq(Some("replacement body")))
        .execute(&mut conn)?;

    assert_eq!(
        stored(&mut conn)?,
        StoredDoc {
            id: 1,
            owner_id: 7,
            body: Some("replacement body".to_owned()),
            note: Some("original note".to_owned()),
        }
    );

    Ok(())
}

/// Both nullable columns nulled in one statement.
#[test]
fn update_sets_every_nullable_column_to_null() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = setup()?;

    diesel::update(docs::table.filter(docs::id.eq(1)))
        .set((docs::body.eq(None::<String>), docs::note.eq(None::<String>)))
        .execute(&mut conn)?;

    assert_eq!(stored(&mut conn)?, StoredDoc { id: 1, owner_id: 7, body: None, note: None });

    Ok(())
}
