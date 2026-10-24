//! `COPY ... FROM STDIN` becomes the `INSERT` of its inline rows, and every
//! other `COPY` is refused rather than discarded.
//!
//! The emitted statements run as text because that text is the artifact under
//! test, and everything read back goes through the typed diesel DSL.

use diesel::{connection::SimpleConnection, prelude::*};
use pg2sqlite::prelude::{Error, Pg2Sqlite, Pg2SqliteOptions};

diesel::define_sql_function! {
    /// Reports whether generated write guards are exempt, true for a restore.
    fn write_is_exempt() -> diesel::sql_types::Bool;
}

mod schema {
    diesel::table! {
        t (rowid) {
            rowid -> BigInt,
            a -> Nullable<Integer>,
            b -> Nullable<Text>,
        }
    }

    diesel::table! {
        g (id) {
            id -> Integer,
            flag -> Bool,
            twice -> Integer,
        }
    }

    diesel::table! {
        notes_rls (id) {
            id -> Integer,
            owner -> Text,
        }
    }

    diesel::table! {
        q (id) {
            id -> Integer,
            #[sql_name = "Odd Name"]
            odd -> Nullable<Text>,
        }
    }
}

const BASE: &str = "CREATE TABLE t (a INTEGER, b TEXT);";

fn translate(pg: &str, options: &Pg2SqliteOptions) -> Result<Vec<String>, Error> {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("the fixture must parse")
        .translate_to_sql(options)
        .map(pg2sqlite::script::Script::into_vec)
}

/// Runs the translation of `pg` on a new database.
fn apply(pg: &str) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    for statement in translate(pg, &Pg2SqliteOptions::default()).expect("translate") {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    connection
}

fn rows(connection: &mut SqliteConnection) -> Vec<(Option<i32>, Option<String>)> {
    schema::t::table
        .select((schema::t::a, schema::t::b))
        .order(schema::t::rowid)
        .load(connection)
        .expect("read t")
}

fn refused(pg: &str) {
    let translated = translate(pg, &Pg2SqliteOptions::default());
    assert!(matches!(translated, Err(Error::TranslationRefusal(_))), "{translated:?}");
}

#[test]
fn a_text_payload_becomes_its_rows() {
    let mut connection = apply(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\t\\N\n2\ty\n\\.\n"));
    assert_eq!(rows(&mut connection), [(Some(1), None), (Some(2), Some("y".to_owned()))]);
}

#[test]
fn text_escapes_decode_and_the_null_string_matches_before_them() {
    let mut connection = apply(&format!(
        "{BASE} COPY t (a, b) FROM stdin;\n\
         1\ttab\\there\n\
         2\t\\101\\x42\\\\\\N\n\
         3\tsplit\\\tfield\n\
         4\t\\\\N\n\
         \\."
    ));
    assert_eq!(
        rows(&mut connection),
        [
            (Some(1), Some("tab\there".to_owned())),
            (Some(2), Some("AB\\N".to_owned())),
            (Some(3), Some("split\tfield".to_owned())),
            (Some(4), Some("\\N".to_owned())),
        ]
    );
}

#[test]
fn a_custom_delimiter_and_null_string_apply() {
    let mut connection = apply(&format!(
        "{BASE} COPY t (a, b) FROM stdin WITH (DELIMITER '|', NULL 'nil');\n1|nil\n2|x\n\\.\n"
    ));
    assert_eq!(rows(&mut connection), [(Some(1), None), (Some(2), Some("x".to_owned()))]);
}

#[test]
fn csv_quotes_carry_delimiters_newlines_and_doubled_quotes() {
    let mut connection = apply(&format!(
        "{BASE} COPY t (a, b) FROM stdin WITH (FORMAT csv, HEADER);\n\
         a,b\n\
         1,\"one, two\"\n\
         2,\"line\nbreak\"\n\
         3,\"say \"\"hi\"\"\"\n\
         4,\n\
         5,\"\"\n\
         \\.\n"
    ));
    assert_eq!(
        rows(&mut connection),
        [
            (Some(1), Some("one, two".to_owned())),
            (Some(2), Some("line\nbreak".to_owned())),
            (Some(3), Some("say \"hi\"".to_owned())),
            (Some(4), None),
            (Some(5), Some(String::new())),
        ]
    );
}

#[test]
fn force_null_and_force_not_null_flip_the_null_match() {
    let mut connection = apply(&format!(
        "{BASE} COPY t (a, b) FROM stdin WITH (FORMAT csv, FORCE_NOT_NULL (b));\n1,\n\\.\n\
         COPY t (a, b) FROM stdin WITH (FORMAT csv, FORCE_NULL (b));\n2,\"\"\n\\.\n"
    ));
    assert_eq!(rows(&mut connection), [(Some(1), Some(String::new())), (Some(2), None)]);
}

#[test]
fn a_copy_without_columns_fills_every_column_but_the_generated_ones() {
    let mut connection = apply(
        "CREATE TABLE g (id INT PRIMARY KEY, flag BOOLEAN NOT NULL, \
         twice INT GENERATED ALWAYS AS (id * 2) STORED);\n\
         COPY g FROM stdin;\n3\tt\n4\tf\n\\.\n",
    );
    let loaded: Vec<(i32, bool, i32)> = schema::g::table
        .select((schema::g::id, schema::g::flag, schema::g::twice))
        .order(schema::g::id)
        .load(&mut connection)
        .expect("read g");
    assert_eq!(loaded, [(3, true, 6), (4, false, 8)]);
}

#[test]
fn an_empty_payload_writes_nothing() {
    let mut connection = apply(&format!("{BASE} COPY t (a, b) FROM stdin;\n\\.\n"));
    assert_eq!(rows(&mut connection), []);
}

#[test]
fn a_secured_table_receives_its_rows_in_the_backing_table() {
    let pg = "CREATE TABLE notes (id INT PRIMARY KEY, owner TEXT NOT NULL);
         ALTER TABLE notes ENABLE ROW LEVEL SECURITY;
         CREATE POLICY notes_read ON notes FOR SELECT USING (false);
         COPY notes (id, owner) FROM stdin;\n1\talice\n\\.\n";
    let mut connection = SqliteConnection::establish(":memory:").expect("connect");
    write_is_exempt_utils::register_impl(&mut connection, || true)
        .expect("register the write exemption");
    let options = Pg2SqliteOptions::default()
        .with_rls_audit_table_name("_rls_audit")
        .with_write_exemption_function("write_is_exempt");
    for statement in translate(pg, &options).expect("translate") {
        connection.batch_execute(&statement).unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    let stored: Vec<(i32, String)> = schema::notes_rls::table
        .select((schema::notes_rls::id, schema::notes_rls::owner))
        .load(&mut connection)
        .expect("read the backing table");
    assert_eq!(stored, [(1, "alice".to_owned())]);
}

#[test]
fn a_row_with_the_wrong_field_count_is_refused() {
    refused(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\tx\textra\n\\.\n"));
    refused(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\n\\.\n"));
}

#[test]
fn a_payload_without_its_terminator_is_refused() {
    refused(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\tx\n"));
}

#[test]
fn an_unterminated_csv_quote_is_refused() {
    refused(&format!("{BASE} COPY t (a, b) FROM stdin WITH (FORMAT csv);\n1,\"open\n\\.\n"));
}

#[test]
fn options_postgresql_refuses_or_the_translator_cannot_decode_are_refused() {
    for options in [
        "(FORMAT binary)",
        "(FORMAT json)",
        "(FREEZE)",
        "(ENCODING 'LATIN1')",
        "(FORCE_QUOTE (a))",
        "(QUOTE '''')",
        "(ESCAPE '|')",
        "(FORCE_NOT_NULL (b))",
        "(FORMAT csv, DELIMITER '\"')",
        "(FORMAT csv, FORCE_NULL (missing))",
        "(FORMAT csv, NULL 'a\"b')",
        "(DELIMITER ',', DELIMITER ';')",
        "(DELIMITER 'a')",
        "(DELIMITER '\\')",
        "(DELIMITER '|', NULL 'a|b')",
    ] {
        refused(&format!("{BASE} COPY t (a, b) FROM stdin WITH {options};\n\\.\n"));
    }
    for legacy in ["BINARY", "CSV FORCE QUOTE a"] {
        refused(&format!("{BASE} COPY t (a, b) FROM stdin {legacy};\n\\.\n"));
    }
}

#[test]
fn the_legacy_option_spellings_apply() {
    let mut connection = apply(&format!(
        "{BASE} COPY t (a, b) FROM stdin WITH DELIMITER '|' NULL 'nil';\n1|nil\n\\.\n\
         COPY t (a, b) FROM stdin WITH CSV HEADER QUOTE '|' ESCAPE '!' FORCE NOT NULL b;\n\
         a,b\n2,\n3,|x!|y!!|\n\\.\n"
    ));
    assert_eq!(
        rows(&mut connection),
        [(Some(1), None), (Some(2), Some(String::new())), (Some(3), Some("x|y!".to_owned()))]
    );
}

#[test]
fn carriage_return_line_endings_are_read_when_every_row_uses_them() {
    let mut connection =
        apply(&format!("{BASE} COPY t (a, b) FROM stdin;\r\n1\tx\r\n2\ty\r\n\\.\r\n"));
    assert_eq!(
        rows(&mut connection),
        [(Some(1), Some("x".to_owned())), (Some(2), Some("y".to_owned()))]
    );
    refused(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\tx\r\n2\ty\n\\.\n"));
}

#[test]
fn short_octal_and_hex_escapes_decode_and_invalid_utf8_is_refused() {
    let mut connection =
        apply(&format!("{BASE} COPY t (a, b) FROM stdin;\n5\t\\x4g\\7z\\xq\n\\.\n"));
    assert_eq!(rows(&mut connection), [(Some(5), Some("\u{4}g\u{7}zxq".to_owned()))]);
    refused(&format!("{BASE} COPY t (a, b) FROM stdin;\n1\t\\377\n\\.\n"));
}

#[test]
fn a_copy_without_columns_into_an_undeclared_table_is_refused() {
    refused("COPY missing FROM stdin;\n\\.\n");
}

/// A file source cannot be read by the translator and SQLite has no equivalent
/// statement.
#[test]
fn copy_from_a_file_is_rejected() {
    refused(&format!("{BASE} COPY t (a, b) FROM '/tmp/rows.csv';"));
}

/// `COPY ... TO` exports rows. Dropping it silently produces no export at all.
#[test]
fn copy_to_stdout_is_rejected() {
    refused(&format!("{BASE} COPY t TO stdout;"));
}

/// `COPY (SELECT ...) TO` is the query form of an export, and is refused for
/// the same reason.
#[test]
fn copy_from_a_query_is_rejected() {
    refused(&format!("{BASE} COPY (SELECT a FROM t) TO stdout;"));
}

#[test]
fn the_control_character_escapes_decode() {
    let mut connection =
        apply(&format!("{BASE} COPY t (a, b) FROM stdin;\n6\t\\b\\f\\n\\r\\v\n\\.\n"));
    assert_eq!(rows(&mut connection), [(Some(6), Some("\u{8}\u{c}\n\r\u{b}".to_owned()))]);
}

#[test]
fn an_explicit_text_format_reads_carriage_return_rows() {
    let mut connection =
        apply(&format!("{BASE} COPY t (a, b) FROM stdin WITH (FORMAT text);\r1\tx\r\\.\r"));
    assert_eq!(rows(&mut connection), [(Some(1), Some("x".to_owned()))]);
}

#[test]
fn a_line_break_in_the_delimiter_or_null_string_is_refused() {
    for options in ["(DELIMITER '\n')", "(NULL 'a\rb')"] {
        refused(&format!("{BASE} COPY t (a, b) FROM stdin WITH {options};\n\\.\n"));
    }
}

#[test]
fn a_quoted_column_is_written_and_matched_by_its_exact_name() {
    let mut connection = apply(
        "CREATE TABLE q (id INT PRIMARY KEY, \"Odd Name\" TEXT);\n\
         COPY q FROM stdin WITH (FORMAT csv, FORCE_NULL (\"Odd Name\"));\n1,\"\"\n\\.\n",
    );
    let loaded: Vec<(i32, Option<String>)> = schema::q::table
        .select((schema::q::id, schema::q::odd))
        .load(&mut connection)
        .expect("read q");
    assert_eq!(loaded, [(1, None)]);
}
