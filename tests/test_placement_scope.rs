//! Placement follows lexical bindings rather than same-named storage.

use std::path::PathBuf;

use diesel::{connection::SimpleConnection, prelude::*, sqlite::SqliteConnection};
use pg2sqlite::{
    prelude::{Error, Pg2Sqlite, Pg2SqliteOptions, Script},
    script::FreshFiles,
};

mod schema {
    diesel::table! {
        log (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        paper_chain (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        virtual_summary (id) {
            id -> Integer,
            title -> Text,
        }
    }

    diesel::table! {
        forward_chain (id) {
            id -> Integer,
        }
    }

    diesel::table! {
        shared.papers (id) {
            id -> Integer,
            parent_id -> Nullable<Integer>,
            title -> Text,
        }
    }
}

use schema::{forward_chain, log, paper_chain, papers as shared_papers, virtual_summary};

const PAPERS: &str = "
    CREATE TABLE papers (id INT PRIMARY KEY, parent_id INT, title TEXT NOT NULL);
    INSERT INTO papers (id, parent_id, title) VALUES (1, NULL, 'seed');
";

/// A seed and a table-fill whose only `papers` reference is a CTE.
const CTE_SEED: &str = "
    CREATE TABLE log AS
        WITH papers AS (SELECT 2 AS id)
        SELECT id FROM papers;
    WITH papers AS (SELECT 3 AS id)
    INSERT INTO log (id)
        SELECT id FROM papers;
";

/// A view whose only `papers` reference is a CTE.
const CTE_VIEW: &str = "
    CREATE VIEW virtual_summary AS
        WITH papers AS (SELECT 2 AS id, 'virtual' AS title)
        SELECT id, title FROM papers;
";

/// A forward recursive sibling named like the attached table. The sibling
/// binds the reference, so the view reads no cross-file storage.
const FORWARD_RECURSIVE_SIBLING: &str = "
    CREATE VIEW forward_chain AS
        WITH RECURSIVE a AS (SELECT id FROM papers), papers AS (SELECT 7 AS id)
        SELECT id FROM a;
";

/// A recursive CTE whose self-reference is lexical and whose base and step
/// read the physical table, beside a nested CTE that shadows the table name.
const PHYSICAL_BESIDE_CTE: &str = "
    INSERT INTO papers (id, parent_id, title) VALUES (100, 99, 'child');
    INSERT INTO papers (id, parent_id, title) VALUES (101, 100, 'grandchild');
    CREATE VIEW paper_chain AS
        SELECT c.id
        FROM (
            WITH RECURSIVE c AS (
                SELECT id FROM papers WHERE parent_id IS NULL
                UNION ALL
                SELECT p.id FROM papers p JOIN c ON p.parent_id = c.id
            )
            SELECT c.id FROM c
        ) c
        WHERE c.id IN (
            SELECT id FROM (
                WITH papers AS (SELECT 99 AS id UNION ALL SELECT 101 AS id)
                SELECT id FROM papers
            ) hidden
        );
";

const PHYSICAL_ROOT: &str = "
    INSERT INTO papers (id, parent_id, title) VALUES (99, NULL, 'root');
";

fn options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default().with_attached_tables("shared", ["papers"])
}

fn translate(pg: &str) -> Result<Script<String>, Error> {
    Pg2Sqlite::default().sql(pg)?.translate_to_sql(&options())
}

/// A main and an attached database file, removed when dropped.
struct Files {
    main: PathBuf,
    shared: PathBuf,
}

impl Files {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir();
        let stem = format!("pg2sqlite-placement-scope-{}-{name}", std::process::id());
        let files = Self {
            main: directory.join(format!("{stem}-main.db")),
            shared: directory.join(format!("{stem}-shared.db")),
        };
        files.remove();
        files
    }

    fn open(&self) -> SqliteConnection {
        SqliteConnection::establish(self.main.to_str().expect("utf-8 path")).expect("open main")
    }

    fn attach(&self, connection: &mut SqliteConnection) {
        connection
            .batch_execute(&format!("ATTACH '{}' AS shared", self.shared.display()))
            .expect("attach shared");
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.main);
        let _ = std::fs::remove_file(&self.shared);
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        self.remove();
    }
}

fn run(connection: &mut SqliteConnection, statements: impl IntoIterator<Item = impl AsRef<str>>) {
    for statement in statements {
        connection
            .batch_execute(statement.as_ref())
            .unwrap_or_else(|error| panic!("{error}\n{}", statement.as_ref()));
    }
}

#[test]
fn a_cte_named_like_an_attached_table_is_no_storage_dependency() {
    let script = translate(&format!("{PAPERS}{CTE_SEED}"))
        .expect("a CTE that names no storage must not refuse the translation");
    let files = Files::new("cte-seed");
    let mut connection = files.open();
    files.attach(&mut connection);
    run(&mut connection, &script.into_vec());

    assert_eq!(
        log::table.select(log::id).order(log::id).load::<i32>(&mut connection).expect("read log"),
        [2, 3]
    );
    assert_eq!(
        shared_papers::table
            .order(shared_papers::id)
            .load::<(i32, Option<i32>, String)>(&mut connection)
            .expect("read the physical table"),
        [(1, None, "seed".to_owned())]
    );
}

#[test]
fn a_view_reading_only_its_cte_lives_in_the_main_file() {
    let script = translate(&format!("{PAPERS}{CTE_VIEW}")).expect("translate");

    let files = Files::new("cte-view-main");
    let mut main_only = files.open();
    run(&mut main_only, script.initialize(FreshFiles::Main).expect("main-file initialization"));
    drop(main_only);
    let mut main_only = files.open();
    assert_eq!(
        virtual_summary::table
            .order(virtual_summary::id)
            .load::<(i32, String)>(&mut main_only)
            .expect("the CTE-only view lives in the main file"),
        [(2, "virtual".to_owned())]
    );

    let script = translate(&format!("{PAPERS}{CTE_VIEW}")).expect("translate");
    let files = Files::new("cte-view-bootstrap");
    let mut connection = files.open();
    files.attach(&mut connection);
    run(&mut connection, &script.into_vec());
    assert_eq!(
        virtual_summary::table
            .order(virtual_summary::id)
            .load::<(i32, String)>(&mut connection)
            .expect("the view still answers after the full bootstrap"),
        [(2, "virtual".to_owned())]
    );
    assert_eq!(
        shared_papers::table
            .order(shared_papers::id)
            .load::<(i32, Option<i32>, String)>(&mut connection)
            .expect("the physical table is untouched"),
        [(1, None, "seed".to_owned())]
    );
}

#[test]
fn a_physical_read_beside_a_shading_cte_keeps_its_storage() {
    let source = format!("{PAPERS}{PHYSICAL_ROOT}{PHYSICAL_BESIDE_CTE}");
    let script = translate(&source).expect("translate");

    let files = Files::new("physical-beside-cte-bootstrapped");
    let mut connection = files.open();
    files.attach(&mut connection);
    run(&mut connection, &script.into_vec());
    assert_eq!(
        paper_chain::table
            .select(paper_chain::id)
            .order(paper_chain::id)
            .load::<i32>(&mut connection)
            .expect("the recursion reads the physical table under the CTEs"),
        [99, 101]
    );
}

#[test]
fn a_forward_recursive_sibling_named_like_an_attached_table_is_lexical() {
    let source = format!("{PAPERS}{FORWARD_RECURSIVE_SIBLING}");
    let script = translate(&source).expect("translate");

    let files = Files::new("forward-sibling-main");
    let mut main_only = files.open();
    run(&mut main_only, script.initialize(FreshFiles::Main).expect("main-file initialization"));
    drop(main_only);
    let mut main_only = files.open();
    assert_eq!(
        forward_chain::table
            .select(forward_chain::id)
            .order(forward_chain::id)
            .load::<i32>(&mut main_only)
            .expect("the sibling bound the forward reference, so the view is persistent"),
        [7]
    );

    let script = translate(&format!("{PAPERS}{FORWARD_RECURSIVE_SIBLING}")).expect("translate");
    let files = Files::new("forward-sibling-bootstrap");
    let mut connection = files.open();
    files.attach(&mut connection);
    run(&mut connection, &script.into_vec());
    assert_eq!(
        forward_chain::table
            .select(forward_chain::id)
            .order(forward_chain::id)
            .load::<i32>(&mut connection)
            .expect("the view still answers after the full bootstrap"),
        [7]
    );
    assert_eq!(
        shared_papers::table
            .order(shared_papers::id)
            .load::<(i32, Option<i32>, String)>(&mut connection)
            .expect("the physical table is untouched"),
        [(1, None, "seed".to_owned())]
    );
}
