//! F8 and F31: a read policy may not consult the table it guards, and a view
//! over such a table bypasses its policy the way PostgreSQL does.
//!
//! Reading a table applies its policy, so a policy that reads the same table
//! needs its own result to produce it. PostgreSQL refuses that outright with
//! `infinite recursion detected in policy for relation`, measured on
//! PostgreSQL 17 as a non-superuser for the plain, CTE and set-operation
//! spellings alike. SQLite has no such detector: it answered `view <table> is
//! circularly defined` where the inner reference kept naming the view, and
//! where the reference was renamed the view worked and filtered, which
//! evaluated input the source database cannot run.
//!
//! Only the read path is refused, which is where PostgreSQL draws the line as
//! well. A `WITH CHECK` predicate and the `USING` predicate of a write-only
//! policy both resolve their inner read under the table's SELECT policy rather
//! than their own, so nothing recurses and PostgreSQL runs them. Measured.
//!
//! F31 is the other half. PostgreSQL runs a view with its owner's rights, so a
//! view over a table with row level security bypasses that policy unless the
//! view says `security_invoker`. Measured on PostgreSQL 17 over a table of
//! three rows whose policy admits two: a plain view answers 3, a
//! `security_invoker` view answers 2. That is what lets a policy consult its
//! own table legitimately, by reading a view instead.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_rls_audit_table_name("rls_violations".to_string())
}

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default().sql(pg).expect("parse").translate_to_sql(&options()).expect("translate")
}

fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&options())
        .expect_err("a self-referential read policy has no evaluable form")
        .to_string()
}

/// Applies every emitted statement and then reads the guarded table, which is
/// where a circular view surfaces.
fn apply_and_read(pg: &str) -> Result<(), diesel::result::Error> {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg) {
        diesel::sql_query(&statement).execute(&mut connection)?;
    }
    diesel::sql_query("SELECT count(*) FROM oo").execute(&mut connection).map(|_| ())
}

const TABLE: &str =
    "CREATE TABLE oo (id INTEGER PRIMARY KEY, owner_id INTEGER, kind INT, shared INT);
     ALTER TABLE oo ENABLE ROW LEVEL SECURITY;";

// ---------------------------------------------------------------------------
// F8: the read path is refused, in every spelling
// ---------------------------------------------------------------------------

/// The plain spelling. This one used to translate and read, filtering rows,
/// which is the case that accepted input PostgreSQL refuses.
#[test]
fn a_plain_self_reference_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (
             EXISTS (SELECT 1 FROM oo d WHERE d.id = oo.id AND d.owner_id > 0));"
    ));
    assert!(error.contains("infinite recursion"), "the refusal must say why: {error}");
    assert!(error.contains("oo"), "the refusal must name the table: {error}");
}

/// The item's first trigger: the reference hides inside a CTE, which the
/// rename never walked, so the view was circular.
#[test]
fn a_self_reference_inside_a_cte_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (
             EXISTS (WITH own AS (SELECT id AS oid FROM oo WHERE owner_id > 0)
                     SELECT 1 FROM own WHERE own.oid = id));"
    ));
    assert!(error.contains("infinite recursion"), "{error}");
}

/// The item's second trigger: a set operation, whose arms the rename never
/// walked either.
#[test]
fn a_self_reference_inside_a_set_operation_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (
             id IN (SELECT id FROM oo WHERE kind = 1
                    UNION ALL SELECT id FROM oo WHERE shared = 1));"
    ));
    assert!(error.contains("infinite recursion"), "{error}");
}

/// A reference reached only through a derived table is still a reference.
#[test]
fn a_self_reference_inside_a_derived_table_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (
             EXISTS (SELECT d.owner_id FROM (SELECT owner_id FROM oo) AS d
                     WHERE d.owner_id = oo.owner_id));"
    ));
    assert!(error.contains("infinite recursion"), "{error}");
}

/// A policy with no `FOR` clause covers every command including the read, and
/// PostgreSQL was measured to recurse on it, so it is refused too.
#[test]
fn a_policy_with_no_command_clause_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo USING (EXISTS (SELECT 1 FROM oo d WHERE d.id = oo.id));"
    ));
    assert!(error.contains("infinite recursion"), "{error}");
}

/// `FOR ALL` likewise.
#[test]
fn a_for_all_policy_is_refused() {
    let error = refusal(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR ALL USING (EXISTS (SELECT 1 FROM oo d WHERE d.id = oo.id));"
    ));
    assert!(error.contains("infinite recursion"), "{error}");
}

// ---------------------------------------------------------------------------
// F8: the write path is not refused, because PostgreSQL evaluates it
// ---------------------------------------------------------------------------

/// A `WITH CHECK` predicate never reaches the view, and PostgreSQL runs it.
#[test]
fn a_self_referential_with_check_is_kept() {
    apply_and_read(&format!(
        "{TABLE}
         CREATE POLICY r ON oo FOR SELECT USING (true);
         CREATE POLICY w ON oo FOR INSERT WITH CHECK (
             EXISTS (SELECT 1 FROM oo d WHERE d.owner_id = oo.owner_id));"
    ))
    .expect("a self-referential WITH CHECK is valid PostgreSQL and must translate");
}

/// A write-only policy's `USING` resolves its inner read under the table's
/// SELECT policy, so PostgreSQL evaluates it and so does this.
#[test]
fn a_self_referential_delete_using_is_kept() {
    apply_and_read(&format!(
        "{TABLE}
         CREATE POLICY r ON oo FOR SELECT USING (true);
         CREATE POLICY d ON oo FOR DELETE USING (
             EXISTS (SELECT 1 FROM oo d2 WHERE d2.owner_id = oo.owner_id));"
    ))
    .expect("a self-referential DELETE USING is valid PostgreSQL and must translate");
}

/// The columns of the guarded table are not a read of it, which matters
/// because nearly every policy mentions them.
#[test]
fn referencing_the_guarded_columns_is_not_a_self_reference() {
    apply_and_read(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (oo.owner_id > 0 AND kind IS NOT NULL);"
    ))
    .expect("a predicate over the table's own columns must translate");
}

// ---------------------------------------------------------------------------
// F31: a view over an RLS table reads the backing table
// ---------------------------------------------------------------------------

/// The legitimate way to write the refused policy, and the reason F31 had to
/// land first: the view bypasses the policy, so the policy may consult it.
#[test]
fn a_policy_may_consult_a_view_over_its_own_table() {
    let pg = format!(
        "{TABLE}
         CREATE VIEW oo_all AS SELECT id, owner_id FROM oo;
         CREATE POLICY p ON oo FOR SELECT USING (
             EXISTS (SELECT 1 FROM oo_all a WHERE a.id = oo.id AND a.owner_id > 0));"
    );
    apply_and_read(&pg).expect("the view breaks the cycle, so this must apply and read");

    let emitted = translate(&pg).join("\n");
    assert!(
        emitted.contains("CREATE VIEW oo_all AS SELECT id, owner_id FROM oo_rls"),
        "the view must read the backing table, not the policy view: {emitted}"
    );
}

/// A view declared `security_invoker` keeps the caller's rights, so it reads
/// the policy view. It used to be refused outright as an unsupported option.
#[test]
fn a_security_invoker_view_keeps_reading_the_policy_view() {
    let emitted = translate(&format!(
        "{TABLE}
         CREATE POLICY p ON oo FOR SELECT USING (owner_id > 0);
         CREATE VIEW oo_seen WITH (security_invoker = true) AS SELECT id, owner_id FROM oo;"
    ))
    .join("\n");
    assert!(
        emitted.contains("CREATE VIEW oo_seen AS SELECT id, owner_id FROM oo"),
        "a security_invoker view reads the policy view: {emitted}"
    );
    assert!(
        !emitted.contains("oo_seen AS SELECT id, owner_id FROM oo_rls"),
        "and must not be retargeted: {emitted}"
    );
}

/// A view over a table with no policies is untouched, so the retarget is
/// scoped to the tables that have a backing table at all.
#[test]
fn a_view_over_an_ordinary_table_is_untouched() {
    let emitted = translate(
        "CREATE TABLE plain (id INTEGER PRIMARY KEY, v TEXT);
         CREATE VIEW plain_all AS SELECT id, v FROM plain;",
    )
    .join("\n");
    assert!(
        emitted.contains("CREATE VIEW plain_all AS SELECT id, v FROM plain"),
        "an ordinary table keeps its name: {emitted}"
    );
}

/// Any other view option stays refused, so honouring `security_invoker` did
/// not open the clause up in general.
#[test]
fn another_view_option_is_still_refused() {
    let error = refusal(
        "CREATE TABLE plain (id INTEGER PRIMARY KEY, v TEXT);
         CREATE VIEW plain_all WITH (check_option = cascaded) AS SELECT id, v FROM plain;",
    );
    assert!(error.contains("VIEW options"), "{error}");
}
