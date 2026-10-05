//! F7: a boolean rendered as text must read `true` or `false`, not `1` or `0`.
//!
//! PostgreSQL renders a boolean cast to text as the words. SQLite has no
//! boolean type, so the translated value is the integer 1 or 0, and
//! `CAST(x AS TEXT)` over it gives `'1'`. Every comparison against `'true'`
//! then silently missed, and `'x' || TRUE` answered `x1`.
//!
//! The rendering is `CASE WHEN x THEN 'true' WHEN NOT x THEN 'false' END`, with
//! no ELSE so a NULL stays NULL. The operand form `CASE x WHEN 1 ... WHEN 0
//! ...` was measured and rejected: a translated boolean column is a bare
//! `INTEGER` with no CHECK, so it can hold 5, and the operand form answers NULL
//! there where PostgreSQL truthiness says true.
//!
//! Every expectation was read off PostgreSQL 17. The emitted statements are
//! executed as text because that text is the artifact under test.

use diesel::prelude::*;
use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions};

mod schema {
    diesel::table! {
        b (id) {
            id -> Integer,
            flag -> Nullable<Integer>,
            n -> Nullable<Integer>,
            t -> Nullable<Text>,
        }
    }
}

const FIXTURE: &str = "CREATE TABLE b (id INT PRIMARY KEY, flag BOOLEAN, n INT, t TEXT);
     INSERT INTO b (id, flag, n, t) VALUES (1, true, 5, 'z'), (2, false, 3, 'y'), (3, NULL, 7, NULL);";

#[derive(QueryableByName)]
struct Rendered {
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    v: Option<String>,
}

fn translate(pg: &str) -> Vec<String> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

/// Every row of `projection`, ordered by id, through the emitted SQLite.
fn rendered(projection: &str) -> Vec<Option<String>> {
    let statements = translate(&format!("{FIXTURE}\nSELECT {projection} AS v FROM b ORDER BY id;"));
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let (probe, setup) = statements.split_last().expect("an emitted query");
    for statement in setup {
        diesel::sql_query(statement)
            .execute(&mut connection)
            .unwrap_or_else(|error| panic!("emitted setup failed: {statement}: {error}"));
    }
    diesel::sql_query(probe)
        .load::<Rendered>(&mut connection)
        .unwrap_or_else(|error| panic!("emitted probe failed: {error}\n{probe}"))
        .into_iter()
        .map(|row| row.v)
        .collect()
}

fn words(projection: &str) -> Vec<Option<&'static str>> {
    rendered(projection)
        .into_iter()
        .map(|value| {
            match value.as_deref() {
                Some("true") => Some("true"),
                Some("false") => Some("false"),
                None => None,
                other => panic!("unexpected rendering {other:?} for {projection}"),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Casts
// ---------------------------------------------------------------------------

/// The item's first trigger, both spellings of the same cast.
#[test]
fn a_boolean_literal_renders_as_a_word() {
    assert_eq!(words("CAST(TRUE AS TEXT)"), vec![Some("true"); 3]);
    assert_eq!(words("TRUE::text"), vec![Some("true"); 3]);
    assert_eq!(words("CAST(FALSE AS TEXT)"), vec![Some("false"); 3]);
}

/// A literal folds straight to the word rather than growing a CASE, since its
/// value is known.
#[test]
fn a_literal_folds_without_a_case() {
    let emitted = translate(&format!("{FIXTURE}\nSELECT CAST(TRUE AS TEXT) AS v FROM b;"));
    let probe = emitted.last().expect("an emitted query");
    assert!(probe.contains("'true'"), "the literal must fold to the word: {probe}");
    assert!(!probe.to_uppercase().contains("CASE"), "no CASE is needed: {probe}");
}

/// A boolean column resolves through its declared type, and the NULL row stays
/// NULL because the rendering has no ELSE.
#[test]
fn a_boolean_column_renders_per_row() {
    assert_eq!(
        words("CAST(flag AS TEXT)"),
        vec![Some("true"), Some("false"), None],
        "true, false, and NULL as PostgreSQL renders them"
    );
}

/// A comparison is boolean by construction, so it renders as a word too.
#[test]
fn a_comparison_result_renders_as_a_word() {
    assert_eq!(words("CAST(n > 4 AS TEXT)"), vec![Some("true"), Some("false"), Some("true")]);
}

/// Every text target type PostgreSQL accepts for the cast renders the same,
/// since they all translate to SQLite TEXT.
#[test]
fn every_text_target_renders_the_same() {
    for target in ["TEXT", "VARCHAR", "VARCHAR(10)", "CHARACTER VARYING(10)"] {
        assert_eq!(
            words(&format!("CAST(flag AS {target})")),
            vec![Some("true"), Some("false"), None],
            "target {target}"
        );
    }
}

// ---------------------------------------------------------------------------
// Concatenation
// ---------------------------------------------------------------------------

/// The item's third trigger. PostgreSQL answers `xtrue`.
#[test]
fn a_boolean_literal_concatenates_as_a_word() {
    assert_eq!(rendered("'x' || TRUE"), vec![Some("xtrue".to_owned()); 3]);
}

/// A boolean column on either side of the operator.
#[test]
fn a_boolean_column_concatenates_as_a_word() {
    assert_eq!(
        rendered("'x' || flag"),
        vec![Some("xtrue".to_owned()), Some("xfalse".to_owned()), None]
    );
    assert_eq!(
        rendered("flag || 'x'"),
        vec![Some("truex".to_owned()), Some("falsex".to_owned()), None]
    );
}

/// The item's second trigger: a comparison against the word now matches.
#[test]
fn a_comparison_against_the_word_matches() {
    assert_eq!(
        rendered("CASE WHEN TRUE::text = 'true' THEN 'hit' ELSE 'miss' END"),
        vec![Some("hit".to_owned()); 3]
    );
}

// ---------------------------------------------------------------------------
// What must not change
// ---------------------------------------------------------------------------

/// Only a boolean operand is rewritten. An integer and a text column keep
/// rendering exactly as they did.
#[test]
fn a_non_boolean_operand_is_untouched() {
    assert_eq!(
        rendered("CAST(n AS TEXT)"),
        vec![Some("5".to_owned()), Some("3".to_owned()), Some("7".to_owned())]
    );
    assert_eq!(rendered("t || 'x'"), vec![Some("zx".to_owned()), Some("yx".to_owned()), None]);
    assert_eq!(
        rendered("n || 'x'"),
        vec![Some("5x".to_owned()), Some("3x".to_owned()), Some("7x".to_owned())]
    );
}

/// Guards the choice of rendering. The translated column is a bare INTEGER, so
/// a client writing through the SQLite side can store a value PostgreSQL's
/// boolean could not hold, and PostgreSQL truthiness makes any nonzero true.
#[test]
fn an_out_of_range_value_is_still_truthy() {
    let statements =
        translate(&format!("{FIXTURE}\nSELECT CAST(flag AS TEXT) AS v FROM b ORDER BY id;"));
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    let (probe, setup) = statements.split_last().expect("an emitted query");
    for statement in setup {
        diesel::sql_query(statement).execute(&mut connection).expect("emitted setup");
    }
    diesel::insert_into(schema::b::table)
        .values((schema::b::id.eq(4), schema::b::flag.eq(5)))
        .execute(&mut connection)
        .expect("a value the INTEGER column accepts");

    let all: Vec<Option<String>> = diesel::sql_query(probe)
        .load::<Rendered>(&mut connection)
        .expect("probe")
        .into_iter()
        .map(|row| row.v)
        .collect();
    assert_eq!(all.last(), Some(&Some("true".to_owned())), "nonzero is true: {all:?}");
}
