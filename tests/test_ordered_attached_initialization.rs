//! Initialization order and fresh-file selection across attached databases.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::*;

mod schema {
    diesel::table! {
        shared.papers (id) {
            id -> Integer,
            body -> Text,
        }
    }
    diesel::table! {
        shared.papers_fts (rowid) {
            rowid -> BigInt,
            body -> Text,
        }
    }
    diesel::table! {
        notes (id) {
            id -> Integer,
        }
    }
    diesel::table! {
        labels (title) {
            title -> Text,
        }
    }
    diesel::table! {
        main.sqlite_schema (name) {
            name -> Text,
        }
    }
    diesel::table! {
        shared.sqlite_master (name) {
            name -> Text,
        }
    }
    diesel::infix_operator!(Matches, " MATCH ", backend: diesel::sqlite::Sqlite);
}

fn translated(sql: &str) -> Script<String> {
    Pg2Sqlite::default()
        .sql(sql)
        .expect("parse")
        .translate_to_sql(&Pg2SqliteOptions::default().with_attached_tables("shared", ["papers"]))
        .expect("translate")
}

fn connection(files: &tempfile::TempDir) -> SqliteConnection {
    let mut connection =
        SqliteConnection::establish(files.path().join("main.db").to_str().expect("path"))
            .expect("open main");
    diesel::sql_query("ATTACH DATABASE ? AS shared")
        .bind::<diesel::sql_types::Text, _>(files.path().join("shared.db").to_str().expect("path"))
        .execute(&mut connection)
        .expect("attach shared");
    connection
}

fn run<'a>(connection: &mut SqliteConnection, statements: impl Iterator<Item = &'a String>) {
    for statement in statements {
        connection.batch_execute(statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
}

#[test]
fn a_failed_source_transaction_rolls_back_both_file_initializers() {
    let sql = "BEGIN;
        CREATE TABLE notes(id INT PRIMARY KEY);
        CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
        INSERT INTO notes VALUES(1);
        INSERT INTO papers VALUES(1, 'first');
        INSERT INTO papers VALUES(1, 'duplicate');
        COMMIT;";
    let script = translated(sql);
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = connection(&files);
    let failure = script
        .initialize(FreshFiles::Both)
        .expect("initialization mode")
        .find_map(|statement| connection.batch_execute(statement).err())
        .expect("duplicate key fails");
    assert!(matches!(
        failure,
        diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::UniqueViolation, _)
    ));
    connection.batch_execute("ROLLBACK").expect("the source transaction remains active");
    let main_tables = schema::sqlite_schema::table
        .filter(schema::sqlite_schema::name.eq("notes"))
        .count()
        .get_result::<i64>(&mut connection)
        .expect("main catalogue");
    let attached_tables = schema::sqlite_master::table
        .filter(schema::sqlite_master::name.eq("papers"))
        .count()
        .get_result::<i64>(&mut connection)
        .expect("attached catalogue");
    assert_eq!((main_tables, attached_tables), (0, 0));
}

#[test]
fn attached_seed_writes_run_with_their_full_text_maintenance_triggers() {
    use diesel::expression::IntoSql;

    let script = translated(
        "CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE INDEX papers_fts ON papers USING GIN(to_tsvector('english', body));
         INSERT INTO papers VALUES(1, 'needle');",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = connection(&files);
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let matches = schema::papers_fts::table
        .filter(schema::Matches::new(
            schema::papers_fts::body,
            "needle".into_sql::<diesel::sql_types::Text>(),
        ))
        .select(schema::papers_fts::rowid)
        .load::<i64>(&mut connection)
        .expect("full-text search");
    assert_eq!(matches, [1]);
}

#[test]
fn a_seed_write_fires_a_temporary_trigger_that_writes_the_attached_file() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE FUNCTION copy_note() RETURNS trigger AS $$
         BEGIN INSERT INTO papers VALUES(NEW.id, 'copied'); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER note_copy AFTER INSERT ON notes FOR EACH ROW
             EXECUTE FUNCTION copy_note();
         INSERT INTO notes VALUES(1);",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = connection(&files);
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let rows = schema::papers::table
        .select((schema::papers::id, schema::papers::body))
        .load::<(i32, String)>(&mut connection)
        .expect("trigger output");
    assert_eq!(rows, [(1, "copied".to_owned())]);
}

#[test]
fn renamed_attached_columns_work_during_initialization_and_reconnect() {
    let script = translated(
        "CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE VIEW labels AS SELECT body AS title FROM papers;
         INSERT INTO papers VALUES(1, 'retained');
         ALTER TABLE papers RENAME COLUMN body TO title;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut connection = connection(&files);
        run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
        let labels = schema::labels::table
            .select(schema::labels::title)
            .load::<String>(&mut connection)
            .expect("view after rename");
        assert_eq!(labels, ["retained"]);
    }
    let mut reopened = connection(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let labels = schema::labels::table
        .select(schema::labels::title)
        .load::<String>(&mut reopened)
        .expect("final view on reopened connection");
    assert_eq!(labels, ["retained"]);
}

#[test]
fn an_existing_shared_file_is_not_initialized_for_a_second_main_file() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         INSERT INTO notes VALUES(7);
         INSERT INTO papers VALUES(1, 'shared');",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    let mut second =
        SqliteConnection::establish(files.path().join("second-main.db").to_str().expect("path"))
            .expect("second main");
    diesel::sql_query("ATTACH DATABASE ? AS shared")
        .bind::<diesel::sql_types::Text, _>(files.path().join("shared.db").to_str().expect("path"))
        .execute(&mut second)
        .expect("attach shared");
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    let notes =
        schema::notes::table.select(schema::notes::id).load::<i32>(&mut second).expect("notes");
    let papers =
        schema::papers::table.select(schema::papers::id).load::<i32>(&mut second).expect("papers");
    assert_eq!((notes, papers), (vec![7], vec![1]));
}

#[test]
fn added_column_foreign_keys_cannot_cross_the_attached_boundary() {
    let sql = "CREATE TABLE parents(id INT PRIMARY KEY);
        CREATE TABLE children(id INT PRIMARY KEY);
        ALTER TABLE children ADD COLUMN parent INT REFERENCES parents(id);";
    for listed in [vec!["parents"], vec!["children"]] {
        let options = Pg2SqliteOptions::default().with_attached_tables("shared", listed);
        let error = Pg2Sqlite::default()
            .sql(sql)
            .expect("parse")
            .translate(&options)
            .expect_err("cross-database key refused");
        assert!(matches!(error, Error::UnusableAttachedTables { .. }), "{error}");
    }
    let options =
        Pg2SqliteOptions::default().with_attached_tables("shared", ["parents", "children"]);
    Pg2Sqlite::default().sql(sql).expect("parse").translate(&options).expect("same database");
}

#[test]
fn an_existing_main_file_can_initialize_a_fresh_attached_store() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         INSERT INTO notes VALUES(7);
         INSERT INTO papers VALUES(1, 'shared');",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    std::fs::remove_file(files.path().join("shared.db")).expect("remove the closed store");
    let mut existing = connection(&files);
    run(
        &mut existing,
        script.initialize(FreshFiles::Attached).expect("attached-only initialization"),
    );
    let notes = schema::notes::table
        .select(schema::notes::id)
        .load::<i32>(&mut existing)
        .expect("existing main rows");
    let papers = schema::papers::table
        .select(schema::papers::id)
        .load::<i32>(&mut existing)
        .expect("new attached rows");
    assert_eq!((notes, papers), (vec![7], vec![1]));
}

#[test]
fn a_new_account_uses_final_column_names_in_an_existing_shared_file() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE VIEW labels AS SELECT body AS title FROM papers;
         INSERT INTO papers VALUES(1, 'retained');
         ALTER TABLE papers RENAME COLUMN body TO title;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    let mut second =
        SqliteConnection::establish(files.path().join("second-main.db").to_str().expect("path"))
            .expect("second main");
    diesel::sql_query("ATTACH DATABASE ? AS shared")
        .bind::<diesel::sql_types::Text, _>(files.path().join("shared.db").to_str().expect("path"))
        .execute(&mut second)
        .expect("attach shared");
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    let labels = schema::labels::table
        .select(schema::labels::title)
        .load::<String>(&mut second)
        .expect("view uses the existing store's final column");
    assert_eq!(labels, ["retained"]);
}

#[test]
fn savepoints_keep_their_selected_writes_inside_the_source_transaction() {
    let script = translated(
        "BEGIN;
         CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         SAVEPOINT seed;
         INSERT INTO notes VALUES(1);
         INSERT INTO papers VALUES(1, 'discarded');
         ROLLBACK TO SAVEPOINT seed;
         INSERT INTO notes VALUES(2);
         INSERT INTO papers VALUES(2, 'retained');
         RELEASE SAVEPOINT seed;
         COMMIT;",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = connection(&files);
    run(&mut connection, script.initialize(FreshFiles::Both).expect("initialize"));
    let notes = schema::notes::table
        .select(schema::notes::id)
        .load::<i32>(&mut connection)
        .expect("main rows");
    let papers = schema::papers::table
        .select(schema::papers::id)
        .load::<i32>(&mut connection)
        .expect("attached rows");
    assert_eq!((notes, papers), (vec![2], vec![2]));
}

#[test]
fn an_unavailable_initializer_is_refused_without_disabling_fresh_files() {
    let script = translated(
        "CREATE TABLE papers(key INT PRIMARY KEY, obsolete INT NOT NULL);
         INSERT INTO papers VALUES(0, 1);
         CREATE TABLE notes AS SELECT obsolete AS id FROM papers;
         ALTER TABLE papers DROP COLUMN obsolete;
         ALTER TABLE papers ADD COLUMN obsolete INT NOT NULL DEFAULT 99;",
    );
    let files = tempfile::TempDir::new().expect("files");
    let mut connection = connection(&files);
    run(&mut connection, script.initialize(FreshFiles::Both).expect("fresh files"));
    let rows = schema::notes::table
        .select(schema::notes::id)
        .load::<i32>(&mut connection)
        .expect("the source snapshot");
    assert_eq!(rows, [1]);
    let failure = script.initialize(FreshFiles::Main).err().expect("unavailable field history");
    assert!(matches!(failure, Error::Initialization(
        pg2sqlite::initialization::InitializationError::Unavailable(
            pg2sqlite::script::ConnectionError::UnavailableColumn { relation, column, .. }
        )
    ) if relation == "papers" && column == "obsolete"));
}

#[test]
fn skipped_attached_history_does_not_disable_fresh_main() {
    let script = Pg2Sqlite::default()
        .sql(
            "CREATE TABLE papers(id INT, doomed INT);
             INSERT INTO papers VALUES(1,7);
             CREATE TABLE snapshot AS SELECT doomed FROM papers;
             ALTER TABLE papers DROP COLUMN doomed;
             CREATE TABLE notes(id INT);
             INSERT INTO notes VALUES(9);",
        )
        .expect("parse")
        .translate_to_sql(
            &Pg2SqliteOptions::default().with_attached_tables("shared", ["papers", "snapshot"]),
        )
        .expect("translate");
    let files = tempfile::tempdir().expect("files");
    {
        let mut creating = connection(&files);
        run(&mut creating, script.initialize(FreshFiles::Both).expect("fresh files"));
        let notes = schema::notes::table
            .select(schema::notes::id)
            .load::<i32>(&mut creating)
            .expect("initial main rows");
        assert_eq!(notes, [9]);
        diesel::update(schema::papers::table)
            .set(schema::papers::id.eq(41))
            .execute(&mut creating)
            .expect("application update to attached storage");
    }
    std::fs::remove_file(files.path().join("main.db")).expect("remove closed main");
    let mut creating = connection(&files);
    run(&mut creating, script.initialize(FreshFiles::Main).expect("independent main program"));
    let notes = schema::notes::table
        .select(schema::notes::id)
        .load::<i32>(&mut creating)
        .expect("fresh main rows");
    let papers = schema::papers::table
        .select(schema::papers::id)
        .load::<i32>(&mut creating)
        .expect("existing attached rows");
    assert_eq!((notes, papers), (vec![9], vec![41]));
}
