//! Focused branch-coverage tests to push total coverage to 95%.

use pg2sqlite::{
    errors::Error,
    prelude::{Pg2SqliteOptions, ReverseTranslator, Translator},
};
use sql_traits::structs::ParserDB;
use sqlparser::{
    ast::{
        ColumnOption, ColumnOptionDef, Expr, Function, FunctionArg, FunctionArgExpr,
        FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, Query, SetExpr,
        Statement, TableObject, Update, UpdateTableFromKind,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
};

fn parse_statements(sql: &str) -> Vec<Statement> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
}

fn parse_statement(sql: &str) -> Statement {
    parse_statements(sql).remove(0)
}

fn parse_query(sql: &str) -> Query {
    let stmt = parse_statement(sql);
    let Statement::Query(query) = stmt else {
        panic!("expected query statement");
    };
    *query
}

fn schema_from_sql(sql: &str) -> ParserDB {
    ParserDB::from_statements(parse_statements(sql), "test".to_string())
        .expect("schema should build from sql")
}

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

fn unsupported_message(err: Error) -> String {
    match err {
        Error::UnsupportedSQLiteFeature(msg) | Error::UnknownPostgresFeature(msg) => msg,
        other => panic!("unexpected error type: {other:?}"),
    }
}

fn simple_function(name: &str) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::ValueWithSpan::from(sqlparser::ast::Value::Number(
                    "1".to_string(),
                    false,
                )),
            )))],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    })
}

#[test]
fn forward_delete_translation_covers_using_join_rls_and_without_keyword_from() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE docs(id INTEGER PRIMARY KEY, owner_id INTEGER, team_id INTEGER);
        CREATE TABLE teams(id INTEGER PRIMARY KEY, owner_id INTEGER);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        ALTER TABLE teams ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT USING (owner_id > 0);
        CREATE POLICY docs_delete ON docs FOR DELETE USING (owner_id > 0);
        CREATE POLICY teams_select ON teams FOR SELECT USING (owner_id > 0);
        "#,
    );
    let options = Pg2SqliteOptions::default();

    let stmt = parse_statement(
        "DELETE FROM docs USING docs d JOIN teams t ON d.team_id = t.id WHERE docs.owner_id = d.owner_id",
    );
    let Statement::Delete(delete) = stmt else {
        panic!("expected DELETE statement");
    };
    let translated_stmt = delete.translate(&schema, &options).expect("delete should translate");
    let Statement::Delete(translated_delete) = translated_stmt else {
        panic!("expected translated DELETE");
    };

    assert!(translated_delete.using.is_none());
    let selection_sql = translated_delete
        .selection
        .as_ref()
        .expect("expected rewritten EXISTS selection")
        .to_string();
    assert!(selection_sql.contains("EXISTS"), "unexpected selection: {selection_sql}");
    assert!(selection_sql.contains("docs_rls"), "unexpected selection: {selection_sql}");
    assert!(selection_sql.contains("teams_rls"), "unexpected selection: {selection_sql}");

    let stmt = parse_statement("DELETE FROM docs WHERE id = 1");
    let Statement::Delete(mut delete) = stmt else {
        panic!("expected DELETE statement");
    };
    let sqlparser::ast::FromTable::WithFromKeyword(tables) = delete.from.clone() else {
        panic!("expected WITH FROM keyword variant");
    };
    delete.from = sqlparser::ast::FromTable::WithoutKeyword(tables);

    let translated_stmt = delete.translate(&schema, &options).expect("delete should translate");
    let Statement::Delete(translated_delete) = translated_stmt else {
        panic!("expected translated DELETE");
    };
    assert!(matches!(translated_delete.from, sqlparser::ast::FromTable::WithoutKeyword(_)));
}

#[test]
fn forward_update_translation_covers_before_set_from_variant() {
    let schema = schema_from_sql(
        "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT); CREATE TABLE teams(id INTEGER PRIMARY KEY, user_id INTEGER);",
    );
    let options = Pg2SqliteOptions::default();

    let stmt =
        parse_statement("UPDATE users SET name = 'x' FROM teams WHERE users.id = teams.user_id");
    let Statement::Update(mut update) = stmt else {
        panic!("expected UPDATE statement");
    };

    let Some(UpdateTableFromKind::AfterSet(tables)) = update.from.take() else {
        panic!("expected AfterSet FROM variant");
    };
    update.from = Some(UpdateTableFromKind::BeforeSet(tables));

    let translated = update.translate(&schema, &options).expect("update should translate");
    assert!(matches!(translated.from, Some(UpdateTableFromKind::BeforeSet(_))));
}

#[test]
fn column_option_translation_covers_unhandled_default_and_option_branches() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    let identifier_default = ColumnOptionDef {
        name: None,
        option: ColumnOption::Default(Expr::Identifier(Ident::new("other_col"))),
    };
    let translated = identifier_default
        .translate(&schema, &options)
        .expect("identifier default should translate")
        .expect("column option should not be filtered");
    assert!(matches!(translated.option, ColumnOption::Default(Expr::Identifier(_))));

    let unsupported_function_default =
        ColumnOptionDef { name: None, option: ColumnOption::Default(simple_function("now")) };
    let err = unsupported_function_default.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Unsupported default expression function"));

    let unsupported_default_expr = ColumnOptionDef {
        name: None,
        option: ColumnOption::Default(Expr::Subquery(Box::new(parse_query("SELECT 1")))),
    };
    let err = unsupported_default_expr.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Unsupported default expression"));

    let unsupported_option =
        ColumnOptionDef { name: None, option: ColumnOption::Comment("column comment".to_string()) };
    let err = unsupported_option.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Unsupported column option"));
}

#[test]
fn statement_translation_covers_if_injection_conditionless_and_trigger_paths() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE audit(id INTEGER PRIMARY KEY);
        CREATE FUNCTION trigger_fn() RETURNS TRIGGER AS $$
        BEGIN
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        "#,
    );
    let options = Pg2SqliteOptions::default();

    let trigger_stmt = parse_statement(
        "CREATE TRIGGER trg_users AFTER INSERT ON users FOR EACH ROW EXECUTE FUNCTION trigger_fn()",
    );
    let translated_trigger =
        trigger_stmt.translate(&schema, &options).expect("trigger statement should translate");
    assert!(!translated_trigger.is_empty(), "expected create trigger statement output");

    let if_stmt = parse_statement(
        "IF TRUE THEN INSERT INTO audit(id) SELECT id FROM users; UPDATE users SET name = 'x'; DELETE FROM users; END IF;",
    );
    let translated_if =
        if_stmt.translate(&schema, &options).expect("simple IF statement should translate");
    assert_eq!(translated_if.len(), 3);
    for stmt in translated_if {
        let sql = stmt.to_string().to_uppercase();
        assert!(sql.contains("TRUE"), "expected injected condition in: {sql}");
    }

    let stmt = parse_statement("IF TRUE THEN DELETE FROM users; END IF;");
    let Statement::If(mut if_stmt) = stmt else {
        panic!("expected IF statement");
    };
    if_stmt.if_block.condition = None;
    let translated = Statement::If(if_stmt)
        .translate(&schema, &options)
        .expect("conditionless IF should be ignored");
    assert!(translated.is_empty());
}

#[test]
fn reverse_statement_and_update_cover_additional_checker_variants() {
    let schema = schema_from_sql(
        "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT); CREATE TABLE teams(id INTEGER PRIMARY KEY, user_id INTEGER);",
    );
    let options = Pg2SqliteOptions::default();

    let insert_stmt = parse_statement("INSERT INTO users(id, name) VALUES (1, 'a')");
    let reversed_insert = insert_stmt
        .reverse_translate(&schema, &options)
        .expect("statement insert reverse translation should succeed");
    assert!(matches!(reversed_insert, Statement::Insert(_)));

    let stmt = parse_statement("UPDATE users SET name = 'x'");
    let Statement::Update(mut update) = stmt else {
        panic!("expected UPDATE statement");
    };

    let join_query = parse_query("SELECT * FROM teams t JOIN users u ON t.user_id = u.id");
    let SetExpr::Select(join_select) = join_query.body.as_ref() else {
        panic!("expected SELECT body");
    };
    let join_from = join_select.from.first().cloned().expect("join query must have one FROM item");

    update.table.joins = join_from.joins.clone();
    update.from = Some(UpdateTableFromKind::BeforeSet(vec![join_from]));

    let reversed_update = Statement::Update(update)
        .reverse_translate(&schema, &options)
        .expect("statement update reverse translation should succeed");
    let Statement::Update(Update { from, .. }) = reversed_update else {
        panic!("expected reversed UPDATE");
    };
    assert!(matches!(from, Some(UpdateTableFromKind::BeforeSet(_))));

    let query_stmt = parse_statement("SELECT * FROM generate_series(1, 2)");
    let reversed_query = query_stmt
        .reverse_translate(&schema, &options)
        .expect("table-function query should reverse");
    assert!(matches!(reversed_query, Statement::Query(_)));

    let substring_query = parse_statement("SELECT SUBSTRING(name FROM 1 FOR 2) FROM users");
    let reversed_substring = substring_query
        .reverse_translate(&schema, &options)
        .expect("substring query should reverse");
    assert!(matches!(reversed_substring, Statement::Query(_)));

    let fn_query = parse_statement("SELECT custom_fn(name) FROM users");
    let reversed_fn_query =
        fn_query.reverse_translate(&schema, &options).expect("function query should reverse");
    assert!(matches!(reversed_fn_query, Statement::Query(_)));

    let bad_stmt = parse_statement("INSERT INTO users(id, name) VALUES (1, 'a')");
    let Statement::Insert(mut insert) = bad_stmt else {
        panic!("expected INSERT statement");
    };
    insert.table = TableObject::TableName(ObjectName(vec![ObjectNamePart::Identifier(
        Ident::new("users_rls"),
    )]));
    let err = Statement::Insert(insert).reverse_translate(&schema, &options).unwrap_err();
    assert!(err.to_string().contains("RLS backing table"));
}
