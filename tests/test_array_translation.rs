//! Translation of PostgreSQL arrays onto the SQLite `json1` extension.
//!
//! Each behavioural assertion is paired with an execution against a real
//! in-memory SQLite so the emitted SQL is proven runnable, not merely
//! plausible. `rusqlite` is used rather than diesel's typed DSL because the
//! statements under test are generated text whose shape is the thing being
//! verified.

#[path = "helpers/run_translated.rs"]
mod run_translated_helper;

use pg2sqlite::prelude::{ArrayRepresentation, Pg2Sqlite, Pg2SqliteOptions, TranslationOptions};
use run_translated_helper::run_translated_with;

/// Options with JSON-backed arrays enabled.
fn json_arrays() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_array_representation(ArrayRepresentation::Json)
}

/// Translate `pg` into the emitted SQLite statements.
fn translate_all(
    pg: &str,
    options: &Pg2SqliteOptions,
) -> Result<Vec<String>, pg2sqlite::errors::Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(options)
}

/// Translate under JSON arrays and join the statements, for shape assertions.
fn translate_json(pg: &str) -> String {
    translate_all(pg, &json_arrays())
        .expect("translation should succeed under JSON arrays")
        .join(";\n")
}

/// Translate under JSON arrays, expecting the message of a rejection.
fn reject_json(pg: &str) -> String {
    translate_all(pg, &json_arrays()).expect_err("translation should be rejected").to_string()
}

/// Translate with no array representation, expecting the message of a
/// rejection.
fn reject_default(pg: &str) -> String {
    translate_all(pg, &Pg2SqliteOptions::default())
        .expect_err("translation should be rejected")
        .to_string()
}

/// Translate a whole PostgreSQL script under JSON arrays and return the first
/// column of its last statement, having applied the rest.
fn run_translated(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &json_arrays())
}

/// A table with an array column plus the seed rows every function test shares.
const TAGS_FIXTURE: &str = "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);
INSERT INTO t (id, tags) VALUES (1, ARRAY['a', 'b', 'a']);
INSERT INTO t (id, tags) VALUES (2, ARRAY[]::TEXT[]);
";

#[test]
fn array_column_needs_a_representation() {
    let err = reject_default("CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);");
    assert!(err.contains("with_array_representation"), "error should name the opt-in: {err}");
}

#[test]
fn array_literal_needs_a_representation() {
    let err = reject_default("SELECT ARRAY[1, 2, 3];");
    assert!(err.contains("with_array_representation"), "error should name the opt-in: {err}");
}

/// Before array support existed the literal was emitted verbatim, producing
/// `SELECT ARRAY[1, 2, 3]`, which SQLite cannot parse. Neither path may leak
/// the PostgreSQL spelling into the output.
#[test]
fn array_literal_is_never_emitted_verbatim() {
    assert!(
        translate_all("SELECT ARRAY[1, 2, 3];", &Pg2SqliteOptions::default()).is_err(),
        "the default must reject rather than emit ARRAY[...]"
    );
    let out = translate_json("SELECT ARRAY[1, 2, 3];");
    assert!(!out.contains("ARRAY["), "translated output must not contain ARRAY[: {out}");
}

#[test]
fn array_columns_become_text() {
    for declaration in ["TEXT[]", "TEXT ARRAY", "TEXT ARRAY[4]"] {
        let out =
            translate_json(&format!("CREATE TABLE t (id INT PRIMARY KEY, tags {declaration});"));
        assert!(out.contains("tags TEXT"), "{declaration} should map to TEXT, got: {out}");
        run_translated_with(
            &format!("CREATE TABLE t (id INT PRIMARY KEY, tags {declaration}); SELECT id FROM t;"),
            &json_arrays(),
        );
    }
}

#[test]
fn array_literal_becomes_json_array() {
    let out = translate_json("SELECT ARRAY[1, 2, 3];");
    assert!(out.contains("json_array(1, 2, 3)"), "got: {out}");
    run_translated("SELECT ARRAY[1, 2, 3];");
}

#[test]
fn array_literal_evaluates_to_a_json_array() {
    assert_eq!(run_translated("SELECT ARRAY[1, 2, 3];"), vec![Some("[1,2,3]".to_string())]);
}

#[test]
fn nested_array_literals_translate_elementwise() {
    let out = translate_json("SELECT ARRAY[ARRAY[1], ARRAY[2]];");
    assert!(out.contains("json_array(json_array(1), json_array(2))"), "got: {out}");
    run_translated("SELECT ARRAY[ARRAY[1], ARRAY[2]];");
}

#[test]
fn literal_subscript_folds_to_a_constant_json_path() {
    let out = translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT tags[2] FROM t;");
    assert!(out.contains("json_extract(tags, '$[1]')"), "got: {out}");
    run_translated("CREATE TABLE t (tags TEXT[]);\nSELECT tags[2] FROM t;");
}

/// PostgreSQL reads a subscript below the lower bound as a miss, so the
/// translation must be NULL rather than a malformed JSON path.
#[test]
fn subscript_below_the_lower_bound_is_null() {
    let out = translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT tags[0] FROM t;");
    assert!(out.contains("NULL"), "got: {out}");
    assert!(!out.contains("$[-1]"), "must not emit a negative JSON path index: {out}");
    run_translated("CREATE TABLE t (tags TEXT[]);\nSELECT tags[0] FROM t;");
}

#[test]
fn subscript_reads_the_expected_element() {
    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT tags[2] FROM t ORDER BY id;")),
        vec![Some("b".to_string()), None],
        "row 1 has a second element, the empty array of row 2 does not"
    );
}

/// A non-literal index has to build the path at runtime, and must still answer
/// NULL rather than error for an index below one.
#[test]
fn computed_subscript_is_guarded() {
    let out = translate_json("CREATE TABLE t (tags TEXT[], i INT);\nSELECT tags[i] FROM t;");
    assert!(out.contains("CASE WHEN i >= 1"), "got: {out}");
    assert!(out.contains("'$[' || (i - 1) || ']'"), "got: {out}");
    run_translated("CREATE TABLE t (tags TEXT[], i INT);\nSELECT tags[i] FROM t;");
}

#[test]
fn computed_subscript_runs_for_in_range_and_out_of_range_indexes() {
    let rows = run_translated(
        "CREATE TABLE t (id INT PRIMARY KEY, i INT, tags TEXT[]);
         INSERT INTO t (id, i, tags) VALUES (1, 2, ARRAY['a', 'b']);
         INSERT INTO t (id, i, tags) VALUES (2, 0, ARRAY['a', 'b']);
         SELECT tags[i] FROM t ORDER BY id;",
    );
    assert_eq!(rows, vec![Some("b".to_string()), None]);
}

#[test]
fn slice_subscript_is_rejected() {
    let err = reject_json("CREATE TABLE t (tags TEXT[]);\nSELECT tags[1:2] FROM t;");
    assert!(err.contains("slice"), "got: {err}");
}

#[test]
fn array_agg_becomes_json_group_array() {
    let out = translate_json("CREATE TABLE t (v INT);\nSELECT array_agg(v) FROM t;");
    assert!(out.contains("json_group_array(v)"), "got: {out}");
    run_translated("CREATE TABLE t (v INT);\nSELECT array_agg(v) FROM t;");
}

#[test]
fn array_agg_accumulates_the_rows() {
    let rows = run_translated(
        "CREATE TABLE t (id INT PRIMARY KEY, v INT);
         INSERT INTO t (id, v) VALUES (1, 1);
         INSERT INTO t (id, v) VALUES (2, 2);
         SELECT array_agg(v) FROM t;",
    );
    assert_eq!(rows, vec![Some("[1,2]".to_string())]);
}

#[test]
fn cardinality_becomes_json_array_length() {
    let out = translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT cardinality(tags) FROM t;");
    assert!(out.contains("json_array_length(tags)"), "got: {out}");
    run_translated("CREATE TABLE t (tags TEXT[]);\nSELECT cardinality(tags) FROM t;");
}

/// PostgreSQL counts an empty array as zero elements.
#[test]
fn cardinality_counts_elements() {
    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT cardinality(tags) FROM t ORDER BY id;")),
        vec![Some("3".to_string()), Some("0".to_string())]
    );
}

/// PostgreSQL reports no upper bound for an empty array while
/// `json_array_length` reports zero, so `array_length` needs the `nullif`
/// guard that `cardinality` does not.
#[test]
fn array_length_reports_null_for_an_empty_array() {
    let out = translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_length(tags, 1) FROM t;");
    assert!(out.contains("nullif(json_array_length(tags), 0)"), "got: {out}");

    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT array_length(tags, 1) FROM t ORDER BY id;")),
        vec![Some("3".to_string()), None]
    );
}

#[test]
fn array_bounds_only_answer_for_the_first_dimension() {
    let err = reject_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_length(tags, 2) FROM t;");
    assert!(err.contains("one-dimensional"), "got: {err}");
}

#[test]
fn array_lower_is_one_for_a_non_empty_array() {
    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT array_lower(tags, 1) FROM t ORDER BY id;")),
        vec![Some("1".to_string()), None]
    );
}

#[test]
fn array_upper_matches_the_element_count() {
    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT array_upper(tags, 1) FROM t ORDER BY id;")),
        vec![Some("3".to_string()), None]
    );
}

#[test]
fn array_to_string_joins_in_element_order() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_to_string(tags, ',') FROM t;");
    assert!(out.contains("group_concat(value, ',' ORDER BY key)"), "got: {out}");
    assert!(out.contains("json_each(tags)"), "got: {out}");

    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_to_string(tags, ',') FROM t ORDER BY id;"
        )),
        vec![Some("a,b,a".to_string()), Some(String::new())],
        "an empty array joins to the empty string, as PostgreSQL does"
    );
}

#[test]
fn array_append_extends_the_array() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_append(tags, 'z') FROM t;");
    assert!(out.contains("json_insert(tags, '$[#]', 'z')"), "got: {out}");

    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_append(tags, 'z') FROM t ORDER BY id;"
        )),
        vec![Some(r#"["a","b","a","z"]"#.to_string()), Some(r#"["z"]"#.to_string())]
    );
}

#[test]
fn array_position_is_one_based_and_null_on_a_miss() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_position(tags, 'b') FROM t;");
    assert!(out.contains("min(key) + 1"), "got: {out}");

    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_position(tags, 'b') FROM t ORDER BY id;"
        )),
        vec![Some("2".to_string()), None]
    );
}

#[test]
fn array_positions_collects_every_match() {
    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_positions(tags, 'a') FROM t ORDER BY id;"
        )),
        vec![Some("[1,3]".to_string()), Some("[]".to_string())]
    );
}

/// `array_remove(a, NULL)` drops NULL elements in PostgreSQL, which a plain
/// `<>` filter would not, so the emitted predicate has to be null-safe.
///
/// The shape is `IS DISTINCT FROM` rather than the `NOT (value IS NULL)` this
/// used to assert, because the helper that builds it stopped emitting the bare
/// `IS` that `sqlparser` cannot read back. SQLite plans the two identically,
/// measured: both answer `SEARCH t USING COVERING INDEX (a>?)`.
#[test]
fn array_remove_is_null_safe() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT array_remove(tags, NULL) FROM t;");
    assert!(out.contains("value IS DISTINCT FROM NULL"), "got: {out}");

    assert_eq!(
        run_translated(
            "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);
             INSERT INTO t (id, tags) VALUES (1, ARRAY['a', NULL, 'b']);
             SELECT array_remove(tags, NULL) FROM t;"
        ),
        vec![Some(r#"["a","b"]"#.to_string())]
    );
}

/// `array_position(a, NULL)` and `array_positions(a, NULL)` find the NULL
/// elements in PostgreSQL, measured on PostgreSQL 16 as 2 and `{2,4}`, so their
/// predicate has to be null safe exactly as `array_remove`'s already was.
#[test]
fn array_position_is_null_safe() {
    const NULLS: &str = "CREATE TABLE t (id INT PRIMARY KEY, tags TEXT[]);
         INSERT INTO t (id, tags) VALUES (1, ARRAY['a', NULL, 'b', NULL]);";

    assert_eq!(
        run_translated(&format!("{NULLS} SELECT array_position(tags, NULL) FROM t;")),
        vec![Some("2".to_string())]
    );
    assert_eq!(
        run_translated(&format!("{NULLS} SELECT array_positions(tags, NULL) FROM t;")),
        vec![Some("[2,4]".to_string())]
    );
}

#[test]
fn array_remove_drops_matching_elements() {
    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_remove(tags, 'a') FROM t ORDER BY id;"
        )),
        vec![Some(r#"["b"]"#.to_string()), Some("[]".to_string())]
    );
}

#[test]
fn array_replace_swaps_matching_elements() {
    assert_eq!(
        run_translated(&format!(
            "{TAGS_FIXTURE}SELECT array_replace(tags, 'a', 'z') FROM t ORDER BY id;"
        )),
        vec![Some(r#"["z","b","z"]"#.to_string()), Some("[]".to_string())]
    );
}

#[test]
fn array_cat_and_array_prepend_are_rejected() {
    for call in ["array_cat(tags, tags)", "array_prepend('z', tags)"] {
        let err = reject_json(&format!("CREATE TABLE t (tags TEXT[]);\nSELECT {call} FROM t;"));
        assert!(err.contains("json_concat"), "got: {err}");
    }
}

#[test]
fn dimension_functions_are_rejected() {
    for call in ["array_dims(tags)", "array_ndims(tags)"] {
        let err = reject_json(&format!("CREATE TABLE t (tags TEXT[]);\nSELECT {call} FROM t;"));
        assert!(err.contains("dimension"), "got: {err}");
    }
}

#[test]
fn unnest_in_from_becomes_a_named_json_each_projection() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT x FROM unnest(ARRAY[1, 2]) AS x;");
    assert!(out.contains("SELECT value AS x FROM json_each(json_array(1, 2))"), "got: {out}");
    run_translated("CREATE TABLE t (tags TEXT[]);\nSELECT x FROM unnest(ARRAY[1, 2]) AS x;");
}

#[test]
fn unnest_output_column_keeps_the_postgres_name() {
    assert_eq!(
        run_translated("SELECT x FROM unnest(ARRAY['a', 'b']) AS x;"),
        vec![Some("a".to_string()), Some("b".to_string())]
    );
}

#[test]
fn unnest_with_ordinality_numbers_from_one() {
    let out = translate_json("SELECT v, i FROM unnest(ARRAY['a']) WITH ORDINALITY AS t(v, i);");
    assert!(out.contains("key + 1 AS i"), "got: {out}");

    assert_eq!(
        run_translated("SELECT i FROM unnest(ARRAY['a', 'b']) WITH ORDINALITY AS t(v, i);"),
        vec![Some("1".to_string()), Some("2".to_string())]
    );
}

#[test]
fn unnest_in_a_select_list_is_rejected() {
    let err = reject_json("CREATE TABLE t (tags TEXT[]);\nSELECT unnest(tags) FROM t;");
    assert!(err.contains("FROM clause"), "got: {err}");
}

/// PostgreSQL reads `FROM t, unnest(t.tags)` as an implicit LATERAL. SQLite has
/// no LATERAL, and the derived table that supplies the PostgreSQL output column
/// name cannot see a sibling FROM item, so a derived table here would emit SQL
/// that fails with "no such column: t.tags".
#[test]
fn unnest_over_a_column_reference_is_rejected() {
    let err = reject_json("CREATE TABLE t (tags TEXT[]);\nSELECT * FROM t, unnest(t.tags) AS tag;");
    assert!(err.contains("LATERAL"), "got: {err}");
}

/// The correlation check has to see through function calls, or a wrapped column
/// reference would slip into the broken derived-table form.
#[test]
fn unnest_over_a_wrapped_column_reference_is_rejected() {
    let err = reject_json(
        "CREATE TABLE t (tags TEXT[]);\nSELECT * FROM t, unnest(coalesce(t.tags, tags)) AS tag;",
    );
    assert!(err.contains("LATERAL"), "got: {err}");
}

/// An `ARRAY[...]` literal operand keeps folding into an `IN` list: that form
/// is exactly faithful and needs no `json_each` scan.
#[test]
fn eq_any_over_a_literal_still_folds_to_in() {
    let out =
        translate_json("CREATE TABLE t (v INT);\nSELECT * FROM t WHERE v = ANY(ARRAY[1, 2]);");
    assert!(out.contains("v IN (1, 2)"), "got: {out}");
    run_translated("CREATE TABLE t (v INT);\nSELECT * FROM t WHERE v = ANY(ARRAY[1, 2]);");
}

#[test]
fn eq_any_over_an_array_column_scans_json_each() {
    let out =
        translate_json("CREATE TABLE t (tags TEXT[]);\nSELECT * FROM t WHERE 'a' = ANY(tags);");
    assert!(out.contains("EXISTS (SELECT 1 FROM json_each(tags) WHERE 'a' = value)"), "got: {out}");
    run_translated("CREATE TABLE t (tags TEXT[]);\nSELECT * FROM t WHERE 'a' = ANY(tags);");
}

#[test]
fn eq_any_over_an_array_column_filters_rows() {
    assert_eq!(
        run_translated(&format!("{TAGS_FIXTURE}SELECT id FROM t WHERE 'b' = ANY(tags);")),
        vec![Some("1".to_string())],
        "only the row whose array contains 'b' survives"
    );
}

#[test]
fn gt_all_over_an_array_column_negates_the_failing_rows() {
    let out =
        translate_json("CREATE TABLE t (v INT, ns INT[]);\nSELECT * FROM t WHERE v > ALL(ns);");
    assert!(out.contains("NOT EXISTS"), "got: {out}");
    assert!(out.contains("IS NOT TRUE"), "got: {out}");
    run_translated("CREATE TABLE t (v INT, ns INT[]);\nSELECT * FROM t WHERE v > ALL(ns);");
}

#[test]
fn gt_all_over_an_array_column_filters_rows() {
    let rows = run_translated(
        "CREATE TABLE t (id INT PRIMARY KEY, v INT, ns INT[]);
         INSERT INTO t (id, v, ns) VALUES (1, 10, ARRAY[1, 2]);
         INSERT INTO t (id, v, ns) VALUES (2, 1, ARRAY[1, 2]);
         SELECT id FROM t WHERE v > ALL(ns);",
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

#[test]
fn quantified_comparison_over_an_array_needs_a_representation() {
    let err = reject_default("CREATE TABLE t (v INT);\nSELECT * FROM t WHERE v = ANY(other);");
    assert!(err.contains("with_array_representation"), "got: {err}");
}

/// A schema, an insert, and a query using arrays throughout must translate and
/// run as one script.
#[test]
fn array_schema_and_query_round_trip_through_sqlite() {
    let rows = run_translated(
        "CREATE TABLE posts (id INT PRIMARY KEY, tags TEXT[] NOT NULL);
         INSERT INTO posts (id, tags) VALUES (1, ARRAY['rust', 'sql']);
         INSERT INTO posts (id, tags) VALUES (2, ARRAY['c']);
         SELECT cardinality(tags) FROM posts WHERE 'rust' = ANY(tags);",
    );
    assert_eq!(rows, vec![Some("2".to_string())]);
}

/// `&&` is true when the two arrays share at least one element.
#[test]
fn overlap_matches_rows_sharing_an_element() {
    let rows = run_translated(&format!("{TAGS_FIXTURE}SELECT id FROM t WHERE tags && ARRAY['a'];"));
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// Disjoint arrays do not overlap, and the empty array in row 2 does not
/// either, which is the boundary worth pinning: an EXISTS over an empty
/// `json_each` must be false rather than vacuously true.
#[test]
fn overlap_excludes_disjoint_and_empty_arrays() {
    let rows = run_translated(&format!("{TAGS_FIXTURE}SELECT id FROM t WHERE tags && ARRAY['z'];"));
    assert!(rows.is_empty(), "nothing overlaps with a disjoint array, got {rows:?}");

    let empty = run_translated("SELECT ARRAY[1, 2] && ARRAY[]::INT[];");
    assert_eq!(empty, vec![Some("0".to_string())]);
}

/// Both operands can be columns, which is the shape the review reproduced.
#[test]
fn overlap_between_two_array_columns() {
    let rows = run_translated(
        "CREATE TABLE t (id INT PRIMARY KEY, a INT[], b INT[]);
         INSERT INTO t (id, a, b) VALUES (1, ARRAY[1, 2], ARRAY[2, 3]);
         INSERT INTO t (id, a, b) VALUES (2, ARRAY[1, 2], ARRAY[8, 9]);
         SELECT id FROM t WHERE a && b;",
    );
    assert_eq!(rows, vec![Some("1".to_string())]);
}

/// Overlap over an array needs the representation opt-in like every other
/// array operation.
#[test]
fn overlap_needs_a_representation() {
    let err = reject_default("SELECT ARRAY[1, 2] && ARRAY[2, 3];");
    assert!(err.contains("with_array_representation"), "error should name the opt-in: {err}");
}

/// Pins the known divergence: PostgreSQL yields NULL when either operand is
/// NULL, and this yields false. Both exclude the row from a `WHERE` clause,
/// which is the only place the difference is observable, the same trade the
/// module header already records for `x <op> ALL(arr)`.
#[test]
fn overlap_with_a_null_operand_is_false_rather_than_null() {
    let rows = run_translated("SELECT NULL::INT[] && ARRAY[1];");
    assert_eq!(rows, vec![Some("0".to_string())]);
}

/// Asking an array column its declared type used to abort the process inside
/// `sql-traits`, which forced every lookup in this crate to read the parsed
/// DDL instead. The prohibition is lifted and this pins it, since a regression
/// would be a process abort rather than a test failure anywhere else.
#[test]
fn an_array_column_answers_its_declared_type() {
    use sql_traits::traits::{ColumnLike, DatabaseLike, TableLike};

    for (ddl, want) in [
        ("CREATE TABLE t (c TEXT[]);", "TEXT[]"),
        ("CREATE TABLE t (c INTEGER[]);", "INT[]"),
        ("CREATE TABLE t (c TEXT[3]);", "TEXT[]"),
        ("CREATE TABLE t (c INT[][]);", "INT[][]"),
    ] {
        let schema = Pg2Sqlite::default().sql(ddl).expect("parses").build_schema().expect("builds");
        let table = schema.tables().next().expect("one table");
        let column = table.columns(&schema).expect("columns").next().expect("one column");
        assert_eq!(column.data_type(&schema), want, "for {ddl}");
    }
}
