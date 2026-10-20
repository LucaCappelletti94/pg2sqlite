//! Replayed queries preserve lexical bindings and output shapes.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{prelude::*, script::ConnectionError};

use super::{open, run, script};

diesel::table! {
    #[sql_name = "seq"]
    seq_view (x) { x -> Integer, }
}

diesel::table! {
    #[sql_name = "v"]
    cte_header_view (x1) { x1 -> Text, }
}

diesel::table! {
    #[sql_name = "v"]
    cte_alias_view (x1) { x1 -> Text, }
}

diesel::table! {
    #[sql_name = "scratch"]
    values_scratch (n) { n -> Integer, body -> Text, }
}

diesel::table! {
    #[sql_name = "v"]
    values_view (column1) { column1 -> Integer, column2 -> Text, }
}

diesel::table! {
    #[sql_name = "joined"]
    joined_view (id) { id -> Integer, y -> Text, }
}

diesel::table! {
    #[sql_name = "derived"]
    derived_shape (y) { y -> Text, }
}

diesel::table! {
    natural_key (id) { id -> Integer, x -> Text, note -> Text, }
}

diesel::table! {
    using_key (id) { id -> Integer, x -> Text, note -> Text, }
}

diesel::table! {
    #[sql_name = "scratch"]
    set_returning_scratch (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    #[sql_name = "v"]
    qualified_view (y) { y -> Text, }
}

diesel::table! {
    #[sql_name = "v"]
    union_view (y) { y -> Text, }
}

diesel::table! {
    #[sql_name = "t"]
    fk_child (id) { id -> Integer, fid -> Integer, }
}

#[test]
fn recursive_cte_shadows_the_physical_relation() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x INT NOT NULL);
         INSERT INTO a VALUES(1, 5);
         CREATE TEMP VIEW seq AS
             WITH RECURSIVE a AS (SELECT 1 AS x UNION ALL SELECT x + 1 FROM a WHERE x < 3)
             SELECT x FROM a;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = seq_view::table
        .select(seq_view::x)
        .order(seq_view::x)
        .load::<i32>(&mut reopened)
        .expect("the replayed recursive view");
    assert_eq!(rows, [1, 2, 3]);
}

#[test]
fn recursive_cte_column_list_pins_the_wildcard_headers() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'lineage');
         CREATE TEMP VIEW v AS
             WITH RECURSIVE q1(id1, x1) AS (SELECT * FROM a), q2 AS (SELECT * FROM q1)
             SELECT x1 FROM q2;
         ALTER TABLE a RENAME TO z;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = cte_header_view::table
        .select(cte_header_view::x1)
        .first::<String>(&mut reopened)
        .expect("the pinned wildcard headers");
    assert_eq!(row, "lineage");
}

#[test]
fn cte_column_list_pins_the_derived_header() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'lineage');
         CREATE TEMP VIEW v AS
             WITH q(x1) AS (SELECT x FROM a)
             SELECT x1 FROM (SELECT x1 AS x1 FROM q) AS d;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = cte_alias_view::table
        .select(cte_alias_view::x1)
        .first::<String>(&mut reopened)
        .expect("the derived header over the cte");
    assert_eq!(row, "lineage");
}

#[test]
fn derived_values_ctas_uses_the_declared_column_names() {
    let script = script(
        "CREATE TEMP TABLE scratch AS SELECT column1 AS n, column2 AS body FROM (VALUES (1, 'a'), (2, 'b'));",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = values_scratch::table
        .order(values_scratch::n)
        .load::<(i32, String)>(&mut reopened)
        .expect("the replayed values rows");
    assert_eq!(rows, [(1, "a".to_owned()), (2, "b".to_owned())]);
}

#[test]
fn values_cte_keeps_the_positional_column_names() {
    let script =
        script("CREATE TEMP VIEW v AS WITH w AS (VALUES (7, 'k')) SELECT column1, column2 FROM w;");
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row =
        values_view::table.first::<(i32, String)>(&mut reopened).expect("the replayed values cte");
    assert_eq!(row, (7, "k".to_owned()));
}

#[test]
fn qualified_wildcards_track_the_renamed_table() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, y TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'ax');
         INSERT INTO b VALUES(1, 'by');
         CREATE TEMP TABLE joined AS SELECT b.* FROM a JOIN b ON a.id = b.id;
         CREATE TEMP TABLE derived AS SELECT d.* FROM (SELECT y AS y FROM b) AS d;
         ALTER TABLE b RENAME TO bz;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let joined =
        joined_view::table.first::<(i32, String)>(&mut reopened).expect("the joined wildcard");
    let derived = derived_shape::table
        .select(derived_shape::y)
        .first::<String>(&mut reopened)
        .expect("the derived wildcard");
    assert_eq!(joined, (1, "by".to_owned()));
    assert_eq!(derived, "by");
}

#[test]
fn natural_and_using_joins_suppress_the_shared_key() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, note TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'ax');
         INSERT INTO b VALUES(1, 'bn');
         CREATE TEMP TABLE natural_key AS SELECT * FROM a NATURAL JOIN b;
         CREATE TEMP TABLE using_key AS SELECT * FROM a JOIN b USING(id);
         ALTER TABLE a RENAME COLUMN x TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let natural = natural_key::table
        .first::<(i32, String, String)>(&mut reopened)
        .expect("the natural join shape");
    let using = using_key::table
        .first::<(i32, String, String)>(&mut reopened)
        .expect("the using join shape");
    assert_eq!(natural, (1, "ax".to_owned(), "bn".to_owned()));
    assert_eq!(using, (1, "ax".to_owned(), "bn".to_owned()));
}

#[test]
fn set_returning_ctas_keeps_the_mapped_output_columns() {
    let script =
        script(r#"CREATE TEMP TABLE scratch AS SELECT * FROM json_each('{"a":"aye","b":"bee"}');"#);
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = set_returning_scratch::table
        .order(set_returning_scratch::key)
        .load::<(String, String)>(&mut reopened)
        .expect("the replayed set-returning columns");
    assert_eq!(
        rows,
        [("a".to_owned(), r#""aye""#.to_owned()), ("b".to_owned(), r#""bee""#.to_owned())]
    );
}

#[test]
fn schema_qualified_names_bind_the_physical_relation() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'qualified');
         CREATE TEMP VIEW v AS SELECT x FROM public.a;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = qualified_view::table
        .select(qualified_view::y)
        .first::<String>(&mut reopened)
        .expect("the schema-qualified view");
    assert_eq!(row, "qualified");
}

#[test]
fn set_operation_view_keeps_the_left_arm_shape() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, note TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'ax');
         INSERT INTO b VALUES(1, 'bn');
         CREATE TEMP VIEW v AS SELECT x FROM a UNION ALL SELECT note FROM b;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = union_view::table
        .select(union_view::y)
        .order(union_view::y)
        .load::<String>(&mut reopened)
        .expect("the union view rows");
    assert_eq!(rows, ["ax".to_owned(), "bn".to_owned()]);
}

#[test]
fn connect_refuses_a_view_over_a_retired_column() {
    let script = script(
        "CREATE TABLE a(key INT PRIMARY KEY, id INT NOT NULL);
         INSERT INTO a VALUES(0, 1);
         CREATE TEMP VIEW v AS SELECT id FROM a;
         ALTER TABLE a DROP COLUMN id;
         ALTER TABLE a ADD COLUMN id INT NOT NULL DEFAULT 99;",
    );
    let error = script.connect().err().expect("the original field is absent");
    assert!(matches!(
        error,
        Error::Connection(ConnectionError::UnavailableColumn {
            relation,
            column,
            statement: 2,
        }) if relation == "a" && column == "id"
    ));
}

#[test]
fn temporary_foreign_key_is_enforced_after_reconnect() {
    let script = script(
        "CREATE TEMP TABLE refd(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO refd VALUES(1, 'r');
         CREATE TEMP TABLE t(id INT PRIMARY KEY, fid INT NOT NULL REFERENCES refd(id));",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    reopened.batch_execute("PRAGMA foreign_keys = ON").expect("enforce the foreign key");
    diesel::insert_into(fk_child::table)
        .values((fk_child::id.eq(1), fk_child::fid.eq(1)))
        .execute(&mut reopened)
        .expect("valid reference");
    let orphaned = diesel::insert_into(fk_child::table)
        .values((fk_child::id.eq(2), fk_child::fid.eq(99)))
        .execute(&mut reopened);
    assert!(matches!(
        orphaned,
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::ForeignKeyViolation,
            _
        ))
    ));
    let rows = fk_child::table
        .order(fk_child::id)
        .load::<(i32, i32)>(&mut reopened)
        .expect("retained references");
    assert_eq!(rows, [(1, 1)]);
}
