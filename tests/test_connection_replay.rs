//! Connection replay preserves temporary state without repeating file
//! initialization.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::{prelude::*, script::ConnectionError};

mod schema {
    diesel::table! {
        scratch (id) { id -> Integer, }
    }
    diesel::table! {
        orders (id) { id -> Integer, amount -> Integer, }
    }
    diesel::table! {
        #[sql_name = "v"]
        renamed (label) { label -> Text, }
    }
    diesel::table! {
        direct (ax) { ax -> Text, bx -> Text, }
    }
    diesel::table! {
        aliased (tx) { tx -> Text, }
    }
    diesel::table! {
        unq (y) { y -> Text, }
    }
    diesel::table! {
        s (id) { id -> Integer, note -> Text, }
    }
    diesel::table! {
        stale (id) { id -> Integer, }
    }
    diesel::table! {
        t (id) { id -> Integer, }
    }
    diesel::table! {
        #[sql_name = "v"]
        sorted (id) { id -> Integer, body -> Integer, }
    }
    diesel::table! {
        nested (bx) { bx -> Text, ax -> Text, }
    }
    diesel::table! {
        cte_scoped (x) { x -> Text, }
    }
    diesel::table! {
        #[sql_name = "v"]
        lineage (x) { x -> Text, }
    }
    diesel::table! {
        #[sql_name = "v"]
        keyword_column (selected) {
            #[sql_name = "select"]
            selected -> Text,
        }
    }
    diesel::table! {
        physical_qualifier (x) { x -> Text, }
    }
    diesel::table! {
        explicit_qualifier (x) { x -> Text, }
    }
    diesel::table! {
        #[sql_name = "a"]
        ctas_source (renamed) { renamed -> Integer, }
    }
}

fn script(sql: &str) -> Script<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default())
        .expect("translate")
}

fn open(files: &tempfile::TempDir) -> SqliteConnection {
    SqliteConnection::establish(files.path().join("main.db").to_str().expect("path")).expect("open")
}

fn run<'a>(connection: &mut SqliteConnection, statements: impl Iterator<Item = &'a String>) {
    for statement in statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

#[test]
fn native_temp_table_and_seed_replay_on_reopen() {
    let script = script(
        "CREATE TABLE orders(id INT PRIMARY KEY, amount INT NOT NULL);
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY);
         INSERT INTO scratch VALUES(1), (2);
         WITH seed AS (SELECT 3 AS id) INSERT INTO scratch SELECT id FROM seed;
         INSERT INTO orders VALUES(1, 10);",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let scratch = schema::scratch::table
        .select(schema::scratch::id)
        .order(schema::scratch::id)
        .load::<i32>(&mut reopened)
        .expect("temporary rows");
    let orders = schema::orders::table.load::<(i32, i32)>(&mut reopened).expect("file rows");
    assert_eq!(scratch, [1, 2, 3]);
    assert_eq!(orders, [(1, 10)]);
}

#[test]
fn temp_view_survives_persistent_column_rename() {
    let script = script(
        "CREATE TABLE source(id INT PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO source VALUES(1, 'kept');
         CREATE TEMP VIEW v AS SELECT name FROM source ORDER BY name;
         ALTER TABLE source RENAME COLUMN name TO label;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::renamed::table
        .select(schema::renamed::label)
        .first::<String>(&mut reopened)
        .expect("renamed view");
    assert_eq!(row, "kept");
}

#[test]
fn rename_rewrites_only_scoped_columns() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'ax');
         INSERT INTO b VALUES(1, 'bx');
         CREATE TEMP VIEW direct AS SELECT a.x AS ax, b.x AS bx FROM a JOIN b ON a.id = b.id;
         CREATE TEMP VIEW aliased AS SELECT t.x AS tx FROM a AS t;
         CREATE TEMP VIEW unq AS SELECT x FROM a;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let direct = schema::direct::table.first::<(String, String)>(&mut reopened).expect("join");
    let aliased = schema::aliased::table
        .select(schema::aliased::tx)
        .first::<String>(&mut reopened)
        .expect("qualified alias");
    let unqualified = schema::unq::table
        .select(schema::unq::y)
        .first::<String>(&mut reopened)
        .expect("unqualified reference");
    assert_eq!(direct, ("ax".to_owned(), "bx".to_owned()));
    assert_eq!(aliased, "ax");
    assert_eq!(unqualified, "ax");
}

#[test]
fn dropped_temp_objects_do_not_leak_into_replay() {
    let script = script(
        "CREATE TEMP TABLE s(id INT PRIMARY KEY);
         INSERT INTO s VALUES(1);
         CREATE TEMP VIEW stale AS SELECT id FROM s;
         DROP VIEW stale;
         DROP TABLE s;
         CREATE TEMP TABLE s(id INT PRIMARY KEY, note TEXT NOT NULL);
         INSERT INTO s VALUES(2, 'fresh');
         CREATE TEMP VIEW stale AS SELECT id FROM s WHERE note = 'fresh';",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::s::table.first::<(i32, String)>(&mut reopened).expect("surviving table");
    let stale = schema::stale::table
        .select(schema::stale::id)
        .first::<i32>(&mut reopened)
        .expect("view resolves the replacement table");
    assert_eq!(row, (2, "fresh".to_owned()));
    assert_eq!(stale, 2);
}

#[test]
fn connection_replay_excludes_rolled_back_rows() {
    let script = script(
        "CREATE TEMP TABLE t(id INT PRIMARY KEY);
         BEGIN;
         INSERT INTO t VALUES(1), (2);
         COMMIT;
         BEGIN;
         INSERT INTO t VALUES(3);
         ROLLBACK;
         INSERT INTO t VALUES(4);",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = schema::t::table
        .select(schema::t::id)
        .order(schema::t::id)
        .load::<i32>(&mut reopened)
        .expect("replayed rows");
    assert_eq!(rows, [1, 2, 4]);
}

#[test]
fn view_alias_and_order_by_keep_the_alias_name() {
    let script = script(
        "CREATE TABLE notes(id INT NOT NULL, body INT NOT NULL);
         INSERT INTO notes VALUES(1, 10), (2, 20);
         CREATE TEMP VIEW v AS SELECT id, -body AS body FROM notes ORDER BY body;
         ALTER TABLE notes RENAME COLUMN body TO value;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = schema::sorted::table.load::<(i32, i32)>(&mut reopened).expect("alias order");
    assert_eq!(rows, [(2, -20), (1, -10)]);
}

#[test]
fn scalar_subquery_bindings_do_not_capture_the_outer_projection() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'outer');
         INSERT INTO b VALUES(1, 'inner');
         CREATE TEMP VIEW nested AS SELECT (SELECT x FROM b) AS bx, x AS ax FROM a;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::nested::table
        .first::<(String, String)>(&mut reopened)
        .expect("isolated nested bindings");
    assert_eq!(row, ("inner".to_owned(), "outer".to_owned()));
}

#[test]
fn cte_bindings_shadow_a_renamed_physical_column() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'physical');
         CREATE TEMP VIEW cte_scoped AS
             WITH a AS (SELECT 'lexical' AS x) SELECT x FROM a;
         ALTER TABLE a RENAME COLUMN x TO y;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::cte_scoped::table
        .select(schema::cte_scoped::x)
        .first::<String>(&mut reopened)
        .expect("the lexical column remains unchanged");
    assert_eq!(row, "lexical");
}

#[test]
fn replay_commits_a_group_before_a_later_statement_fails() {
    let script = script(
        "CREATE TEMP TABLE t(id INT PRIMARY KEY);
         BEGIN;
         INSERT INTO t VALUES(1);
         COMMIT;
         INSERT INTO t VALUES(1);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut reopened = open(&files);
    let error = script
        .connect()
        .expect("connection program")
        .find_map(|statement| reopened.batch_execute(statement).err())
        .expect("the duplicate insert fails");
    assert!(error.to_string().contains("UNIQUE constraint failed"), "{error}");
    let rollback = reopened.batch_execute("ROLLBACK");
    let rows = schema::t::table
        .select(schema::t::id)
        .load::<i32>(&mut reopened)
        .expect("the committed row");
    assert_eq!(rows, [1]);
    assert!(rollback.is_err(), "the committed transaction is already closed");
}

#[test]
fn external_schema_columns_bind_connection_definitions() {
    let ddl = "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
               INSERT INTO a VALUES(1, 'external');";
    let initializer = script(ddl);
    let schema = Pg2Sqlite::default()
        .sql(ddl)
        .expect("external source")
        .build_schema()
        .expect("external schema");
    let partial = Pg2Sqlite::default()
        .sql(
            "CREATE TEMP VIEW v AS SELECT x FROM a;
             ALTER TABLE a RENAME COLUMN x TO label;
             CREATE INDEX a_label ON a(label);",
        )
        .expect("partial source")
        .translate_to_sql_with_schema(&schema, &Pg2SqliteOptions::default())
        .expect("partial translation");
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, initializer.iter());
        run(&mut creating, partial.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, partial.connect().expect("connection program"));
    let row = schema::renamed::table
        .select(schema::renamed::label)
        .first::<String>(&mut reopened)
        .expect("the external column binding");
    assert_eq!(row, "external");
}

fn assert_replayed_alias(definition: &str) {
    let script = script(&format!(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'lineage');
         {definition};
         ALTER TABLE a RENAME COLUMN x TO y;"
    ));
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let row = schema::lineage::table
            .select(schema::lineage::x)
            .first::<String>(&mut creating)
            .expect("the initialized output name");
        assert_eq!(row, "lineage");
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::lineage::table
        .select(schema::lineage::x)
        .first::<String>(&mut reopened)
        .expect("the replayed output name");
    assert_eq!(row, "lineage");
}

#[test]
fn derived_projection_preserves_explicit_output_names() {
    assert_replayed_alias("CREATE TEMP VIEW v AS SELECT d.x FROM (SELECT x AS x FROM a) AS d");
}

#[test]
fn cte_projection_preserves_explicit_output_names() {
    assert_replayed_alias("CREATE TEMP VIEW v AS WITH q AS (SELECT x AS x FROM a) SELECT x FROM q");
}

#[test]
fn temporary_view_dependencies_preserve_explicit_output_names() {
    assert_replayed_alias(
        "CREATE TEMP VIEW base AS SELECT x AS x FROM a;
         CREATE TEMP VIEW v AS SELECT x FROM base",
    );
}

#[test]
fn persistent_view_dependencies_preserve_explicit_output_names() {
    assert_replayed_alias(
        "CREATE VIEW base AS SELECT x AS x FROM a;
         CREATE TEMP VIEW v AS SELECT x FROM base",
    );
}

#[test]
fn renamed_table_qualifiers_preserve_explicit_aliases() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'qualified');
         CREATE TEMP VIEW physical_qualifier AS SELECT a.x FROM a;
         CREATE TEMP VIEW explicit_qualifier AS SELECT a.x FROM a AS a;
         ALTER TABLE a RENAME TO z;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let direct = schema::physical_qualifier::table
        .select(schema::physical_qualifier::x)
        .first::<String>(&mut reopened)
        .expect("the physical qualifier");
    let aliased = schema::explicit_qualifier::table
        .select(schema::explicit_qualifier::x)
        .first::<String>(&mut reopened)
        .expect("the explicit alias");
    assert_eq!(direct, "qualified");
    assert_eq!(aliased, "qualified");
}

#[test]
fn renamed_columns_remain_quoted_when_required() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'keyword');
         CREATE TEMP VIEW v AS SELECT x FROM a;
         ALTER TABLE a RENAME COLUMN x TO \"select\";",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::keyword_column::table
        .select(schema::keyword_column::selected)
        .first::<String>(&mut reopened)
        .expect("the quoted output column");
    assert_eq!(row, "keyword");
}

#[test]
fn replayed_ctas_consumes_the_current_persistent_rows() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY);
         INSERT INTO a VALUES(1);
         CREATE TEMP TABLE scratch AS SELECT id FROM a;
         ALTER TABLE a RENAME COLUMN id TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let rows = schema::scratch::table
        .select(schema::scratch::id)
        .order(schema::scratch::id)
        .load::<i32>(&mut reopened)
        .expect("the replayed seed");
    assert_eq!(rows, [1]);
    {
        let mut mutator = open(&files);
        diesel::insert_into(schema::ctas_source::table)
            .values(schema::ctas_source::renamed.eq(7))
            .execute(&mut mutator)
            .expect("the persistent row");
    }
    let mut reseeded = open(&files);
    run(&mut reseeded, script.connect().expect("connection program"));
    let rows = schema::scratch::table
        .select(schema::scratch::id)
        .order(schema::scratch::id)
        .load::<i32>(&mut reseeded)
        .expect("the current seed");
    assert_eq!(rows, [1, 7]);
}

#[test]
fn connect_refuses_replay_over_a_dropped_source() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY);
         INSERT INTO a VALUES(1);
         CREATE TEMP TABLE scratch AS SELECT id FROM a;
         DROP TABLE a;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let error = script.connect().err().expect("the dropped source cannot be replayed");
    match error {
        Error::Connection(ConnectionError::UnavailableRelation { relation, statement }) => {
            assert_eq!(relation, "a");
            assert_eq!(statement, 2);
        }
        other => panic!("the wrong connection error {other:?}"),
    }
}

#[test]
fn replayed_ctas_output_survives_a_renamed_view_base() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY);
         INSERT INTO a VALUES(1);
         CREATE VIEW base AS SELECT id FROM a;
         CREATE TEMP TABLE scratch AS SELECT id FROM base;
         ALTER TABLE a RENAME COLUMN id TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::scratch::table
        .select(schema::scratch::id)
        .first::<i32>(&mut reopened)
        .expect("the stored output name");
    assert_eq!(row, 1);
}

#[test]
fn replayed_temp_view_output_survives_later_column_addition() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY);
         CREATE TABLE b(dummy INT NOT NULL);
         INSERT INTO a VALUES(1);
         INSERT INTO b VALUES(0);
         CREATE TEMP VIEW scratch AS SELECT (SELECT id FROM b) AS id FROM a;
         ALTER TABLE a RENAME COLUMN id TO renamed;
         ALTER TABLE b ADD COLUMN id INT NOT NULL DEFAULT 99;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
        let row = schema::scratch::table
            .select(schema::scratch::id)
            .first::<i32>(&mut creating)
            .expect("the fresh output name");
        assert_eq!(row, 1);
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::scratch::table
        .select(schema::scratch::id)
        .first::<i32>(&mut reopened)
        .expect("the replayed output name");
    assert_eq!(row, 1);
}

#[test]
fn renamed_column_referenced_only_in_join_on() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY, x TEXT NOT NULL);
         CREATE TABLE b(id INT PRIMARY KEY, x TEXT NOT NULL);
         INSERT INTO a VALUES(1, 'ax');
         INSERT INTO b VALUES(1, 'bx');
         CREATE TEMP VIEW direct AS SELECT a.x AS ax, b.x AS bx FROM a JOIN b ON a.id = b.id;
         ALTER TABLE a RENAME COLUMN id TO renamed;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let row = schema::direct::table
        .first::<(String, String)>(&mut reopened)
        .expect("the joined projection");
    assert_eq!(row, ("ax".to_owned(), "bx".to_owned()));
}

#[test]
fn replay_keeps_the_temporary_history_required_by_surviving_rows() {
    for sql in [
        "CREATE TEMP TABLE staging(id INT PRIMARY KEY);
         INSERT INTO staging VALUES(7);
         CREATE TEMP TABLE scratch AS SELECT id FROM staging;
         DROP TABLE staging;",
        "CREATE TABLE a(id INT PRIMARY KEY);
         INSERT INTO a VALUES(1);
         CREATE TEMP TABLE staging AS SELECT id FROM a;
         DROP TABLE staging;
         DROP TABLE a;
         CREATE TEMP TABLE scratch(id INT PRIMARY KEY);
         INSERT INTO scratch VALUES(7);",
    ] {
        let script = script(sql);
        let files = tempfile::TempDir::new().expect("files");
        {
            let mut creating = open(&files);
            run(&mut creating, script.iter());
        }
        let mut reopened = open(&files);
        run(&mut reopened, script.connect().expect("connection program"));
        let rows = schema::scratch::table
            .select(schema::scratch::id)
            .load::<i32>(&mut reopened)
            .expect("surviving snapshot");
        assert_eq!(rows, [7]);
    }
}

#[test]
fn replay_refuses_a_retired_field_even_when_its_name_is_reused() {
    let script = script(
        "CREATE TABLE a(key INT PRIMARY KEY, id INT NOT NULL);
         INSERT INTO a VALUES(0, 1);
         CREATE TEMP TABLE scratch AS SELECT id FROM a;
         ALTER TABLE a DROP COLUMN id;
         ALTER TABLE a ADD COLUMN id INT NOT NULL DEFAULT 99;",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut creating = open(&files);
    run(&mut creating, script.iter());
    let row = schema::scratch::table
        .select(schema::scratch::id)
        .first::<i32>(&mut creating)
        .expect("source snapshot");
    assert_eq!(row, 1);
    let error = script.connect().err().expect("the original field is absent");
    assert!(matches!(error, Error::Connection(ConnectionError::UnavailableColumn {
        relation, column, statement: 2,
    }) if relation == "a" && column == "id"));
}

#[test]
fn replayed_ctas_wildcard_retains_its_source_column_shape() {
    let script = script(
        "CREATE TABLE a(id INT PRIMARY KEY);
         INSERT INTO a VALUES(1);
         CREATE TEMP TABLE scratch AS SELECT * FROM a;
         ALTER TABLE a RENAME COLUMN id TO renamed;
         ALTER TABLE a ADD COLUMN future INT NOT NULL DEFAULT 99;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = open(&files);
        run(&mut creating, script.iter());
    }
    let mut reopened = open(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    reopened.batch_execute("INSERT INTO scratch VALUES(7)").expect("source column count");
    let rows = schema::scratch::table
        .select(schema::scratch::id)
        .order(schema::scratch::id)
        .load::<i32>(&mut reopened)
        .expect("source columns");
    assert_eq!(rows, [1, 7]);
}

#[test]
fn replayed_ctas_wildcard_preserves_merged_outer_join_values() {
    for (join, expected) in [("RIGHT JOIN", vec![2]), ("FULL JOIN", vec![1, 2])] {
        let script = script(&format!(
            "CREATE TABLE a(id INT PRIMARY KEY);
             CREATE TABLE b(id INT PRIMARY KEY);
             INSERT INTO a VALUES(1);
             INSERT INTO b VALUES(2);
             CREATE TEMP TABLE scratch AS SELECT * FROM a {join} b USING(id);"
        ));
        let files = tempfile::TempDir::new().expect("files");
        {
            let mut creating = open(&files);
            run(&mut creating, script.iter());
        }
        let mut reopened = open(&files);
        run(&mut reopened, script.connect().expect("connection program"));
        let rows = schema::scratch::table
            .select(schema::scratch::id)
            .order(schema::scratch::id)
            .load::<i32>(&mut reopened)
            .expect("merged keys");
        assert_eq!(rows, expected);
    }
}

#[path = "connection_replay/headers.rs"]
mod header_coverage;

#[path = "connection_replay/lifecycle.rs"]
mod lifecycle_coverage;

#[path = "connection_replay/transactions.rs"]
mod transactions;

#[path = "connection_replay/dml.rs"]
mod dml;
