//! `json_typeof` and `jsonb_typeof` against SQLite's `json_type`.
//!
//! The two functions answer over different vocabularies, so a comparison
//! against a PostgreSQL type name never matches. Measured on PostgreSQL 16 and
//! SQLite 3.51.1:
//!
//! | input | `json_type` | `json_typeof` |
//! |---|---|---|
//! | `"x"` | `text` | `string` |
//! | `1` | `integer` | `number` |
//! | `1.5` | `real` | `number` |
//! | `true` | `true` | `boolean` |
//! | `false` | `false` | `boolean` |
//! | `null` | `null` | `null` |
//! | `{}` | `object` | `object` |
//! | `[]` | `array` | `array` |
//!
//! Both answer SQL NULL for a SQL NULL argument.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::Pg2SqliteOptions;
use run_translated_helper::run_translated_with;

/// Every value in SQLite's `json_type` domain, plus SQL NULL last.
const TABLE: &str = r#"CREATE TABLE t (id INT PRIMARY KEY, v JSONB);
     INSERT INTO t VALUES (1, '"x"'), (2, '1'), (3, '1.5'), (4, 'true'),
                          (5, 'false'), (6, 'null'), (7, '{}'), (8, '[]'), (9, NULL);"#;

fn postgres_answers() -> Vec<Option<String>> {
    ["string", "number", "number", "boolean", "boolean", "null", "object", "array"]
        .iter()
        .map(|name| Some((*name).to_string()))
        .chain([None])
        .collect()
}

#[test]
fn json_typeof_answers_the_postgres_vocabulary() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT json_typeof(v) FROM t ORDER BY id;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, postgres_answers());
}

#[test]
fn jsonb_typeof_answers_the_same() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT jsonb_typeof(v) FROM t ORDER BY id;"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, postgres_answers());
}

/// The point of the item: a comparison against a PostgreSQL type name has to
/// match. `number` covers both of SQLite's numeric answers.
#[test]
fn comparing_against_a_postgres_type_name_matches() {
    let rows = run_translated_with(
        &format!("{TABLE} SELECT count(*) FROM t WHERE json_typeof(v) = 'number';"),
        &Pg2SqliteOptions::default(),
    );
    assert_eq!(rows, vec![Some("2".to_string())], "the integer and the real are both numbers");
}
