//! What reverse translation counts as a reference to an RLS backing table.
//!
//! The refusal exists because a backing table has no PostgreSQL counterpart:
//! forward translation renames the secured table and puts a view in its place,
//! so a SQLite statement naming `docs_rls` names an object PostgreSQL does not
//! have. Before this suite the refusal compared the written name against the
//! suffix with `ends_with`, which made it both defeatable and wrong:
//!
//! - `docs_RLS` and `"DOCS_RLS"` passed through, though SQLite reaches the same
//!   table through any spelling, so a caller escaped the refusal by holding
//!   shift.
//! - A table the schema declares as `audit_rls`, carrying no row level security
//!   at all, could not be read back, though forward translation emits it
//!   verbatim.
//!
//! The suffix is now the fallback rather than the rule: the schema says which
//! tables carry row level security, and therefore which physical names the
//! forward direction produced.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2Sqlite, Pg2SqliteOptions},
};

/// A secured table, so the schema knows `docs` is served by a view over
/// `docs_rls`.
const SECURED: &str = "
CREATE TABLE docs (id INT PRIMARY KEY, owner TEXT NOT NULL, secret TEXT);
ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY docs_owner ON docs USING (owner = current_user);
";

/// A table whose name ends with the default suffix and which carries no row
/// level security, which forward translation emits under that exact name.
const PLAIN_WITH_SUFFIXED_NAME: &str = "CREATE TABLE audit_rls (id INT PRIMARY KEY, note TEXT);";

fn reverse(sqlite: &str, ddl: &str, options: &Pg2SqliteOptions) -> Result<String, Error> {
    let translator = Pg2Sqlite::default().sql(ddl)?;
    let schema = translator.build_schema()?;
    let statements = translator.reverse_sql(sqlite, &schema, options)?;
    Ok(statements.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
}

fn refusal(sqlite: &str, ddl: &str, options: &Pg2SqliteOptions) -> Error {
    match reverse(sqlite, ddl, options) {
        Ok(emitted) => panic!("reverse translation should have refused, emitted: {emitted}"),
        Err(error) => error,
    }
}

#[test]
fn a_lower_case_reference_to_a_backing_table_is_refused() {
    let error = refusal("SELECT * FROM docs_rls", SECURED, &Pg2SqliteOptions::default());
    assert!(
        matches!(&error, Error::RlsTableDetected { table_name, .. } if table_name == "docs_rls"),
        "the refusal should name the backing table, got: {error:?}"
    );
}

#[test]
fn a_mixed_case_reference_to_a_backing_table_is_refused() {
    let error = refusal("SELECT * FROM docs_RLS", SECURED, &Pg2SqliteOptions::default());
    assert!(
        matches!(&error, Error::RlsTableDetected { .. }),
        "SQLite reaches the backing table through any spelling, got: {error:?}"
    );
}

#[test]
fn a_quoted_upper_case_reference_to_a_backing_table_is_refused() {
    let error = refusal(r#"DELETE FROM "DOCS_RLS""#, SECURED, &Pg2SqliteOptions::default());
    assert!(
        matches!(&error, Error::RlsTableDetected { .. }),
        "quoting does not make a SQLite name case sensitive, got: {error:?}"
    );
}

#[test]
fn a_mixed_case_reference_inside_a_subquery_is_refused() {
    let error = refusal(
        "SELECT * FROM docs WHERE id IN (SELECT id FROM Docs_Rls)",
        SECURED,
        &Pg2SqliteOptions::default(),
    );
    assert!(
        matches!(&error, Error::RlsTableDetected { .. }),
        "a nested reference reaches the same table, got: {error:?}"
    );
}

#[test]
fn a_mixed_case_reference_under_a_custom_suffix_is_refused() {
    let options = Pg2SqliteOptions::default().with_rls_table_suffix("_secure");
    let error = refusal("SELECT * FROM docs_SECURE", SECURED, &options);
    assert!(
        matches!(&error, Error::RlsTableDetected { suffix, .. } if suffix == "_secure"),
        "the refusal should name the configured suffix, got: {error:?}"
    );
}

#[test]
fn a_declared_table_named_like_a_backing_table_reverses() {
    let emitted = reverse(
        "SELECT * FROM audit_rls",
        PLAIN_WITH_SUFFIXED_NAME,
        &Pg2SqliteOptions::default(),
    )
    .expect("a table with no row level security has a PostgreSQL counterpart under its own name");
    assert_eq!(emitted, "SELECT * FROM audit_rls");
}

#[test]
fn a_declared_table_named_like_a_backing_table_is_emitted_under_that_name() {
    let emitted = Pg2Sqlite::default()
        .sql(PLAIN_WITH_SUFFIXED_NAME)
        .expect("schema parses")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("a plain table translates");
    assert_eq!(
        emitted,
        ["CREATE TABLE audit_rls (id INTEGER PRIMARY KEY NOT NULL, note TEXT) STRICT"]
    );
}

#[test]
fn an_undeclared_name_carrying_the_suffix_is_refused() {
    let error = refusal(
        "SELECT * FROM users_rls",
        "CREATE TABLE other (id INT PRIMARY KEY);",
        &Pg2SqliteOptions::default(),
    );
    assert!(
        matches!(&error, Error::RlsTableDetected { .. }),
        "a name the schema does not declare falls back to the suffix rule, got: {error:?}"
    );
}

#[test]
fn a_cte_alias_shadowing_a_backing_table_only_by_case_is_refused() {
    let error = refusal(
        r#"WITH "DOCS_RLS" AS (SELECT 1 AS id) SELECT * FROM docs_rls"#,
        SECURED,
        &Pg2SqliteOptions::default(),
    );
    let message = error.to_string();
    assert!(
        message.contains("DOCS_RLS") && message.contains("PostgreSQL"),
        "the refusal should name the alias and say the two databases read it differently, got: \
         {message}"
    );
}

#[test]
fn a_cte_alias_matching_a_backing_table_exactly_still_shadows_it() {
    let emitted = reverse(
        "WITH docs_rls AS (SELECT 1 AS id) SELECT * FROM docs_rls",
        SECURED,
        &Pg2SqliteOptions::default(),
    )
    .expect("both databases bind an unquoted reference to an unquoted alias");
    assert_eq!(emitted, "WITH docs_rls AS (SELECT 1 AS id) SELECT * FROM docs_rls");
}

#[test]
fn a_cte_alias_does_not_shadow_the_same_name_inside_its_body() {
    let error = refusal(
        "WITH docs_rls AS (SELECT id FROM docs_rls) SELECT * FROM docs_rls",
        SECURED,
        &Pg2SqliteOptions::default(),
    );
    assert!(
        matches!(&error, Error::RlsTableDetected { .. }),
        "the CTE body still reads the backing table, got: {error:?}"
    );
}

#[test]
fn an_empty_suffix_is_refused_when_translating_forward() {
    let error = Pg2Sqlite::default()
        .sql("CREATE TABLE t (id INT PRIMARY KEY);")
        .expect("schema parses")
        .translate_to_sql(&Pg2SqliteOptions::default().with_rls_table_suffix(""))
        .expect_err("an empty suffix cannot separate a backing table from its view");
    assert!(
        matches!(&error, Error::EmptyRlsTableSuffix),
        "the refusal should name the setting, got: {error:?}"
    );
}

#[test]
fn an_empty_suffix_is_refused_when_translating_in_reverse() {
    let error = refusal(
        "SELECT * FROM other",
        "CREATE TABLE other (id INT PRIMARY KEY);",
        &Pg2SqliteOptions::default().with_rls_table_suffix(""),
    );
    assert!(
        matches!(&error, Error::EmptyRlsTableSuffix),
        "an empty suffix would read every name as a backing table, got: {error:?}"
    );
}
