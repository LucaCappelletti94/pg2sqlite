//! Initialization order and fresh-file selection across attached databases.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::*;

mod schema {
    diesel::table! {
        shared.papers (id) {
            id -> Integer,
            body -> Text,
            note -> Integer,
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

    diesel::table! {
        records (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        joins (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        archive (id) {
            id -> Integer,
            title -> Text,
        }
    }
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

/// A connection to a new main file with the existing shared file attached.
fn second_connection(files: &tempfile::TempDir) -> SqliteConnection {
    let mut connection =
        SqliteConnection::establish(files.path().join("second-main.db").to_str().expect("path"))
            .expect("second main");
    diesel::sql_query("ATTACH DATABASE ? AS shared")
        .bind::<diesel::sql_types::Text, _>(files.path().join("shared.db").to_str().expect("path"))
        .execute(&mut connection)
        .expect("attach shared");
    connection
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
    let mut second = second_connection(&files);
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    let notes =
        schema::notes::table.select(schema::notes::id).load::<i32>(&mut second).expect("notes");
    let papers =
        schema::papers::table.select(schema::papers::id).load::<i32>(&mut second).expect("papers");
    assert_eq!((notes, papers), (vec![7], vec![1]));
}

#[test]
fn a_second_main_file_maintains_full_text_search_in_an_existing_shared_file() {
    use diesel::expression::IntoSql;

    let script = translated(
        "CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE INDEX papers_fts ON papers USING GIN(to_tsvector('english', body));",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    let mut second = second_connection(&files);
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    diesel::insert_into(schema::papers::table)
        .values((schema::papers::id.eq(1), schema::papers::body.eq("needle")))
        .execute(&mut second)
        .expect("write the shared table");
    let matches = schema::papers_fts::table
        .filter(schema::Matches::new(
            schema::papers_fts::body,
            "needle".into_sql::<diesel::sql_types::Text>(),
        ))
        .select(schema::papers_fts::rowid)
        .load::<i64>(&mut second)
        .expect("full-text search");
    assert_eq!(matches, [1]);
}

#[test]
fn a_second_main_file_fires_a_trigger_on_an_existing_shared_table() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE FUNCTION note_paper() RETURNS trigger AS $$
         BEGIN INSERT INTO notes VALUES(NEW.id); RETURN NEW; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER paper_note AFTER INSERT ON papers FOR EACH ROW
             EXECUTE FUNCTION note_paper();",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    let mut second = second_connection(&files);
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    diesel::insert_into(schema::papers::table)
        .values((schema::papers::id.eq(3), schema::papers::body.eq("noted")))
        .execute(&mut second)
        .expect("write the shared table");
    let notes =
        schema::notes::table.select(schema::notes::id).load::<i32>(&mut second).expect("notes");
    assert_eq!(notes, [3]);
}

#[test]
fn a_second_main_file_initializes_a_script_that_creates_a_role() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE ROLE reader;
         GRANT SELECT ON papers TO reader;
         INSERT INTO notes VALUES(7);
         INSERT INTO papers VALUES(1, 'shared');",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    let mut second = second_connection(&files);
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
fn a_fresh_attached_store_next_to_an_existing_main_file_keeps_full_text_search() {
    use diesel::expression::IntoSql;

    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         CREATE INDEX papers_fts ON papers USING GIN(to_tsvector('english', body));
         INSERT INTO notes VALUES(7);
         INSERT INTO papers VALUES(1, 'needle');",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    std::fs::remove_file(files.path().join("shared.db")).expect("remove the closed store");
    let mut existing = connection(&files);
    let initialization: Vec<&String> =
        script.initialize(FreshFiles::Attached).expect("attached-only initialization").collect();
    run(&mut existing, initialization.into_iter());
    let matches = schema::papers_fts::table
        .filter(schema::Matches::new(
            schema::papers_fts::body,
            "needle".into_sql::<diesel::sql_types::Text>(),
        ))
        .select(schema::papers_fts::rowid)
        .load::<i64>(&mut existing)
        .expect("full-text search");
    assert_eq!(matches, [1]);
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
    let mut second = second_connection(&files);
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

#[test]
fn a_fresh_attached_store_binds_a_renamed_main_object_to_its_final_name() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, note INT NOT NULL);
         CREATE VIEW joins AS SELECT n.id FROM papers p JOIN notes n ON n.id = p.note;
         ALTER TABLE notes RENAME TO records;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    std::fs::remove_file(files.path().join("shared.db")).expect("remove the closed store");
    let mut second = connection(&files);
    run(
        &mut second,
        script.initialize(FreshFiles::Attached).expect("attached-only initialization"),
    );
    diesel::insert_into(schema::records::table)
        .values(schema::records::id.eq(1))
        .execute(&mut second)
        .expect("main row");
    diesel::insert_into(schema::papers::table)
        .values((schema::papers::id.eq(10), schema::papers::note.eq(1)))
        .execute(&mut second)
        .expect("attached row");
    let ids = schema::joins::table
        .select(schema::joins::id)
        .load::<i32>(&mut second)
        .expect("the view uses the existing store's final name");
    assert_eq!(ids, [1]);
}

#[test]
fn a_fresh_main_store_binds_a_renamed_attached_column_to_its_final_name() {
    let script = translated(
        "CREATE TABLE papers(id INT PRIMARY KEY, body TEXT NOT NULL);
         INSERT INTO papers VALUES(1, 'retained');
         CREATE TABLE archive AS SELECT id, body AS title FROM papers;
         ALTER TABLE papers RENAME COLUMN body TO content;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut first = connection(&files);
        run(&mut first, script.initialize(FreshFiles::Both).expect("initialize"));
    }
    std::fs::remove_file(files.path().join("main.db")).expect("remove the closed main");
    let mut second = connection(&files);
    run(&mut second, script.initialize(FreshFiles::Main).expect("main-only initialization"));
    let archive = schema::archive::table
        .select((schema::archive::id, schema::archive::title))
        .load::<(i32, String)>(&mut second)
        .expect("the table fill reads the final attached column");
    assert_eq!(archive, [(1, "retained".to_owned())]);
}

#[test]
fn a_fresh_attached_initializer_missing_main_history_is_refused_on_its_own() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         INSERT INTO notes VALUES(1);
         CREATE TABLE papers(id INT PRIMARY KEY, note INT);
         INSERT INTO papers SELECT id, id FROM notes;
         DROP TABLE notes;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = connection(&files);
        run(&mut creating, script.initialize(FreshFiles::Both).expect("fresh files"));
        let papers = schema::papers::table
            .select(schema::papers::id)
            .load::<i32>(&mut creating)
            .expect("attached rows");
        assert_eq!(papers, [1]);
    }
    let failure = script.initialize(FreshFiles::Attached).err().expect("unavailable main history");
    assert!(matches!(failure, Error::Initialization(
        pg2sqlite::initialization::InitializationError::Unavailable(
            pg2sqlite::script::ConnectionError::UnavailableRelation { relation, .. }
        )
    ) if relation == "notes"));
    std::fs::remove_file(files.path().join("main.db")).expect("remove the closed main");
    let mut fresh = connection(&files);
    run(&mut fresh, script.initialize(FreshFiles::Main).expect("independent main program"));
    let notes = schema::sqlite_schema::table
        .filter(schema::sqlite_schema::name.eq("notes"))
        .count()
        .get_result::<i64>(&mut fresh)
        .expect("main catalogue");
    let papers = schema::papers::table
        .select(schema::papers::id)
        .load::<i32>(&mut fresh)
        .expect("existing attached rows");
    assert_eq!((notes, papers), (0, vec![1]));
}

#[test]
fn a_reconnect_rebinds_a_temp_view_over_a_renamed_main_object() {
    let script = translated(
        "CREATE TABLE notes(id INT PRIMARY KEY);
         CREATE TABLE papers(id INT PRIMARY KEY, note INT NOT NULL);
         CREATE VIEW joins AS SELECT n.id FROM papers p JOIN notes n ON n.id = p.note;
         ALTER TABLE notes RENAME TO records;",
    );
    let files = tempfile::TempDir::new().expect("files");
    {
        let mut creating = connection(&files);
        run(&mut creating, script.initialize(FreshFiles::Both).expect("initialize"));
        diesel::insert_into(schema::records::table)
            .values(schema::records::id.eq(1))
            .execute(&mut creating)
            .expect("main row");
        diesel::insert_into(schema::papers::table)
            .values((schema::papers::id.eq(10), schema::papers::note.eq(1)))
            .execute(&mut creating)
            .expect("attached row");
    }
    let mut reopened = connection(&files);
    run(&mut reopened, script.connect().expect("connection program"));
    let ids = schema::joins::table
        .select(schema::joins::id)
        .load::<i32>(&mut reopened)
        .expect("the replayed temp view reads the renamed main table");
    assert_eq!(ids, [1]);
}
