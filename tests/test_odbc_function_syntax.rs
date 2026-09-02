//! ODBC function escape refusal tests.

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use pg2sqlite::{
    options::TranslationContext,
    prelude::{Pg2Sqlite, Pg2SqliteOptions, ReverseTranslator, Translator},
};
use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{Expr, Function},
    dialect::PostgreSqlDialect,
    parser::Parser,
};

fn odbc_function() -> Function {
    let expression = Parser::new(&PostgreSqlDialect {})
        .try_with_sql("{fn ABS(-1)}")
        .expect("configure parser")
        .parse_expr()
        .expect("parse ODBC function");
    let Expr::Function(function) = expression else {
        panic!("expected function");
    };
    function
}

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("build empty schema")
}

#[test]
fn forward_refuses_odbc_function_escape_syntax() {
    let error = odbc_function()
        .translate(&empty_schema(), &Pg2SqliteOptions::default())
        .expect_err("ODBC function escapes must be refused");
    assert!(error.to_string().contains("ODBC function escape syntax"), "unexpected error: {error}");
}

#[test]
fn reverse_refuses_odbc_function_escape_syntax() {
    let options = Pg2SqliteOptions::default();
    let error = Expr::Function(odbc_function())
        .reverse_translate(&empty_schema(), &TranslationContext::new(&options))
        .expect_err("ODBC function escapes must be refused");
    assert!(error.to_string().contains("ODBC function escape syntax"), "unexpected error: {error}");
}

#[test]
fn forward_translation_never_emits_the_sqlite_invalid_escape() {
    let mut connection = SqliteConnection::establish(":memory:").expect("open SQLite");
    let sqlite_error = diesel::sql_query("SELECT {fn ABS(-1)}")
        .execute(&mut connection)
        .expect_err("SQLite must reject ODBC escape syntax")
        .to_string();
    assert!(
        sqlite_error.contains("unrecognized token") || sqlite_error.contains("near \"{\""),
        "unexpected SQLite error: {sqlite_error}"
    );

    let error = Pg2Sqlite::default()
        .sql("SELECT {fn ABS(-1)}")
        .expect("parse ODBC function")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("translation must refuse SQL that SQLite cannot parse");
    assert!(error.to_string().contains("ODBC function escape syntax"), "unexpected error: {error}");
}
