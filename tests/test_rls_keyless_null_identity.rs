//! A keyless guarded table whose rows carry NULL must be updatable and
//! deletable through its RLS view.
//!
//! When no primary key exists the write triggers fall back to identifying
//! the backing-table row by comparing every column against OLD. The
//! comparison used to be `=`, and `NULL = NULL` evaluates to NULL (never
//! true), so any row holding NULL in any column was invisible to the
//! trigger's forwarding query. Fix: use `IS NOT DISTINCT FROM`, which
//! treats two NULLs as equal and is otherwise identical to `=`.

mod helpers;

use diesel::prelude::*;
use helpers::establish_connection;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};

mod schema {
    diesel::table! {
        // The RLS view. `label` serves as the diesel primary key only to
        // satisfy table!'s requirement; the SQL table has no PK.
        t (label) {
            label -> Text,
            owner -> Text,
            note -> Nullable<Text>,
        }
    }

    diesel::table! {
        // Backing table. Read directly so assertions see what was stored.
        t_rls (label) {
            label -> Text,
            owner -> Text,
            note -> Nullable<Text>,
        }
    }

    diesel::table! {
        k (id) {
            id -> Integer,
            owner -> Text,
            note -> Nullable<Text>,
        }
    }

    diesel::table! {
        k_rls (id) {
            id -> Integer,
            owner -> Text,
            note -> Nullable<Text>,
        }
    }
}

use schema::{k, k_rls, t, t_rls};

// Keyless table: no PRIMARY KEY, one nullable column.
const KEYLESS: &str = "
    CREATE TABLE t (label TEXT NOT NULL, owner TEXT NOT NULL, note TEXT);
    ALTER TABLE t ENABLE ROW LEVEL SECURITY;
    CREATE POLICY p ON t USING (owner = 'alice');
";

// Keyed table: INTEGER PRIMARY KEY, one nullable column.
const KEYED: &str = "
    CREATE TABLE k (id INTEGER PRIMARY KEY, owner TEXT NOT NULL, note TEXT);
    ALTER TABLE k ENABLE ROW LEVEL SECURITY;
    CREATE POLICY p ON k USING (owner = 'alice');
";

/// Translates `pg` and applies the result as DDL. The emitted SQL is the
/// artifact under test so it runs as text; every other statement uses the
/// typed DSL.
fn apply(pg: &str) -> SqliteConnection {
    let translated = Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit"))
        .expect("translate");

    let mut conn = establish_connection();
    for statement in &translated {
        // DDL cannot be expressed with the typed DSL; sql_query is
        // intentional here.
        diesel::sql_query(statement.to_string())
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("emitted DDL failed: {e}\n{statement}"));
    }
    conn
}

/// An update through the RLS view of a row whose nullable column holds NULL
/// must land. Before the fix, NULL = NULL was always NULL (never true) in the
/// trigger's backing-table WHERE, so the row was silently unchanged.
#[test]
fn update_of_null_bearing_row_lands() {
    let mut conn = apply(KEYLESS);

    // Insert directly into the backing table so note is genuinely NULL.
    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert into backing table");

    diesel::update(t::table)
        .filter(t::label.eq("a").and(t::note.is_null()))
        .set(t::label.eq("b"))
        .execute(&mut conn)
        .expect("update through view must not error");

    let (label, note): (String, Option<String>) = t_rls::table
        .select((t_rls::label, t_rls::note))
        .first(&mut conn)
        .expect("row must exist in backing table");

    assert_eq!(label, "b", "the update must have landed in the backing table");
    assert_eq!(note, None, "the null note must remain null after the update");
}

/// A delete through the RLS view of a row whose nullable column holds NULL
/// must remove it.
#[test]
fn delete_of_null_bearing_row_removes_it() {
    let mut conn = apply(KEYLESS);

    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert into backing table");

    diesel::delete(t::table.filter(t::label.eq("a").and(t::note.is_null())))
        .execute(&mut conn)
        .expect("delete through view must not error");

    let count: i64 = t_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 0, "the delete must have removed the row from the backing table");
}

/// An update targeting a null-note row must not disturb a row that shares the
/// same label but carries a non-null note. This proves the identity clause
/// narrows to the correct row rather than widening into everything.
#[test]
fn bystander_with_non_null_note_survives_update() {
    let mut conn = apply(KEYLESS);

    // Both rows share label='a'; only the note column distinguishes them.
    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert null-note row");
    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice"), t_rls::note.eq("set")))
        .execute(&mut conn)
        .expect("insert non-null-note row");

    diesel::update(t::table)
        .filter(t::label.eq("a").and(t::note.is_null()))
        .set(t::label.eq("upd"))
        .execute(&mut conn)
        .expect("update through view must not error");

    // Sort by note ASC so NULL sorts first, giving a predictable order.
    let rows: Vec<(String, Option<String>)> = t_rls::table
        .select((t_rls::label, t_rls::note))
        .order_by(t_rls::note.asc())
        .load(&mut conn)
        .expect("select all rows from backing table");

    assert_eq!(rows.len(), 2, "both rows must still exist");
    assert_eq!(rows[0], ("upd".to_owned(), None), "the null-note row must carry the new label");
    assert_eq!(
        rows[1],
        ("a".to_owned(), Some("set".to_owned())),
        "the non-null-note row must be untouched"
    );
}

/// A delete targeting a null-note row must not disturb a row that shares the
/// same label but carries a non-null note.
#[test]
fn bystander_with_non_null_note_survives_delete() {
    let mut conn = apply(KEYLESS);

    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert null-note row");
    diesel::insert_into(t_rls::table)
        .values((t_rls::label.eq("a"), t_rls::owner.eq("alice"), t_rls::note.eq("set")))
        .execute(&mut conn)
        .expect("insert non-null-note row");

    diesel::delete(t::table.filter(t::label.eq("a").and(t::note.is_null())))
        .execute(&mut conn)
        .expect("delete through view must not error");

    let count: i64 = t_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 1, "only the null-note row must have been deleted");

    let (remaining_note,): (Option<String>,) =
        t_rls::table.select((t_rls::note,)).first(&mut conn).expect("remaining row");
    assert_eq!(
        remaining_note,
        Some("set".to_owned()),
        "the surviving row must be the one with the non-null note"
    );
}

/// Keyed tables use `=` on the primary key, which cannot be NULL. The fix
/// must not change their behaviour.
#[test]
fn keyed_table_update_and_delete_are_unaffected() {
    let mut conn = apply(KEYED);

    diesel::insert_into(k_rls::table)
        .values((k_rls::id.eq(1), k_rls::owner.eq("alice")))
        .execute(&mut conn)
        .expect("insert row 1 with null note");
    diesel::insert_into(k_rls::table)
        .values((k_rls::id.eq(2), k_rls::owner.eq("alice"), k_rls::note.eq("set")))
        .execute(&mut conn)
        .expect("insert row 2");

    // Update the null-note row by its primary key.
    diesel::update(k::table.find(1))
        .set(k::note.eq("updated"))
        .execute(&mut conn)
        .expect("update through keyed view must succeed");

    let (updated_note,): (Option<String>,) = k_rls::table
        .select((k_rls::note,))
        .filter(k_rls::id.eq(1))
        .first(&mut conn)
        .expect("row 1");
    assert_eq!(updated_note, Some("updated".to_owned()), "keyed update must land");

    // Delete the non-null-note row.
    diesel::delete(k::table.find(2))
        .execute(&mut conn)
        .expect("delete through keyed view must succeed");

    let count: i64 = k_rls::table.count().get_result(&mut conn).expect("count");
    assert_eq!(count, 1, "only row 2 must have been deleted");
}
