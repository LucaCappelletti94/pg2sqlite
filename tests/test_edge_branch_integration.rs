//! Integration tests for edge branches across view, index, trigger, RLS,
//! function, and reverse translation modules.

use pg2sqlite::{
    errors::Error,
    impls::translator_impls::rls::{
        generate_readonly_rls_statements, generate_rls_statements,
        generate_rls_validation_statements, generate_rls_view_sql, resolve_trigger_table_name,
        table_has_rls,
    },
    options::Pg2SqliteOptions,
    prelude::{ReverseTranslator, Translator},
    traits::{Schema as _, TranslationOptions},
};
use sql_traits::{structs::ParserDB, traits::DatabaseLike};
use sqlparser::{
    ast::{
        ConditionalStatements, ConstraintCharacteristics, CreateFunctionBody, CreateIndex,
        CreateTableOptions, CreateTrigger, CreateView, Expr, Function, FunctionArg,
        FunctionArgExpr, FunctionArgOperator, FunctionArgumentList, FunctionArguments, Ident,
        ObjectName, ObjectNamePart, Query, SetExpr, SqlOption, Statement, TableObject,
        TriggerExecBodyType,
    },
    dialect::PostgreSqlDialect,
    parser::Parser,
    tokenizer::Span,
};

fn parse_statements(sql: &str) -> Vec<Statement> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
}

fn parse_statement(sql: &str) -> Statement {
    parse_statements(sql).remove(0)
}

fn parse_query(sql: &str) -> Query {
    let stmt = parse_statement(sql);
    if let Statement::Query(query) = stmt {
        *query
    } else {
        panic!("expected query statement");
    }
}

fn parse_insert(sql: &str) -> sqlparser::ast::Insert {
    let stmt = parse_statement(sql);
    if let Statement::Insert(insert) = stmt {
        insert
    } else {
        panic!("expected INSERT statement");
    }
}

fn parse_expr(sql: &str) -> Expr {
    Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
}

fn parse_create_view(sql: &str) -> CreateView {
    let stmt = parse_statement(sql);
    if let Statement::CreateView(view) = stmt {
        view
    } else {
        panic!("expected CREATE VIEW");
    }
}

fn parse_create_index(sql: &str) -> CreateIndex {
    let stmt = parse_statement(sql);
    if let Statement::CreateIndex(index) = stmt {
        index
    } else {
        panic!("expected CREATE INDEX");
    }
}

fn parse_create_trigger(sql: &str) -> CreateTrigger {
    let stmt = parse_statement(sql);
    if let Statement::CreateTrigger(trigger) = stmt {
        trigger
    } else {
        panic!("expected CREATE TRIGGER");
    }
}

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

fn schema_from_sql(sql: &str) -> ParserDB {
    ParserDB::from_statements(parse_statements(sql), "test".to_string())
        .expect("schema should build from sql")
}

fn unsupported_message(err: Error) -> String {
    match err {
        Error::UnsupportedSQLiteFeature(msg) | Error::UnknownPostgresFeature(msg) => msg,
        other => panic!("unexpected error type: {other:?}"),
    }
}

#[test]
fn create_view_translation_rejects_additional_unsupported_variants() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let base = parse_create_view("CREATE VIEW v AS SELECT 1");

    let mut or_alter = base.clone();
    or_alter.or_alter = true;
    assert!(
        unsupported_message(or_alter.translate(&schema, &options).unwrap_err())
            .contains("CREATE OR ALTER VIEW")
    );

    let mut secure = base.clone();
    secure.secure = true;
    assert!(
        unsupported_message(secure.translate(&schema, &options).unwrap_err())
            .contains("SECURE VIEW")
    );

    let mut with_options = base.clone();
    with_options.options = CreateTableOptions::With(vec![SqlOption::Ident(Ident::new("dummy"))]);
    assert!(
        unsupported_message(with_options.translate(&schema, &options).unwrap_err())
            .contains("VIEW options")
    );

    let mut cluster_by = base.clone();
    cluster_by.cluster_by = vec![Ident::new("id")];
    assert!(
        unsupported_message(cluster_by.translate(&schema, &options).unwrap_err())
            .contains("CLUSTER BY")
    );

    let mut to_clause = base.clone();
    to_clause.to = Some(ObjectName(vec![ObjectNamePart::Identifier(Ident::new("sink"))]));
    assert!(
        unsupported_message(to_clause.translate(&schema, &options).unwrap_err())
            .contains("TO clause")
    );

    let mut no_schema_binding = base;
    no_schema_binding.with_no_schema_binding = true;
    assert!(
        unsupported_message(no_schema_binding.translate(&schema, &options).unwrap_err())
            .contains("WITH NO SCHEMA BINDING")
    );
}

#[test]
fn create_index_translation_covers_gin_error_branches() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
        CREATE TABLE docs_composite(
            id INTEGER,
            tenant_id INTEGER,
            title TEXT,
            PRIMARY KEY(id, tenant_id)
        );
        "#,
    );
    let options = Pg2SqliteOptions::default();

    let unsupported_expr = parse_create_index("CREATE INDEX idx_bad ON docs USING GIN (title)");
    let err = unsupported_expr.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Only to_tsvector()"));

    let no_columns = parse_create_index(
        "CREATE INDEX idx_empty ON docs USING GIN (to_tsvector('english', 'literal'))",
    );
    let err = no_columns.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Only to_tsvector()"));

    let mut empty_columns =
        parse_create_index("CREATE INDEX idx_no_cols ON docs USING GIN (to_tsvector(title))");
    empty_columns.columns.clear();
    let err = empty_columns.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("must reference at least one column"));

    let missing_table = parse_create_index(
        "CREATE INDEX idx_missing ON missing_docs USING GIN (to_tsvector(title))",
    );
    let err = missing_table.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Could not find table"));

    let composite_pk = parse_create_index(
        "CREATE INDEX idx_composite ON docs_composite USING GIN (to_tsvector(title))",
    );
    let err = composite_pk.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("single-column primary key"));
}

#[test]
fn create_index_translation_covers_nested_function_extraction_paths() {
    let schema =
        schema_from_sql("CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT);");
    let options = Pg2SqliteOptions::default();

    let nested = parse_create_index(
        "CREATE INDEX idx_nested ON docs USING GIN (to_tsvector(lower(title) || body))",
    );
    let statements = nested.translate(&schema, &options).expect("fts index should translate");
    assert!(
        statements.iter().any(|s| matches!(s, Statement::CreateVirtualTable { .. })),
        "expected FTS5 virtual table"
    );
    assert!(
        statements.iter().any(|s| matches!(s, Statement::CreateTrigger { .. })),
        "expected FTS5 sync triggers"
    );

    let mut none_args =
        parse_create_index("CREATE INDEX idx_none ON docs USING GIN (to_tsvector(title))");
    if let Expr::Function(func) = &mut none_args.columns[0].column.expr {
        func.args = FunctionArguments::None;
    }
    let err = none_args.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Only to_tsvector()"));
}

#[test]
fn create_trigger_translation_rejects_unsupported_shapes_and_handles_missing_function_body() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);
        CREATE FUNCTION trigger_fn() RETURNS TRIGGER AS $$
        BEGIN
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        "#,
    );
    let schema_without_fn =
        schema_from_sql("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT);");
    let options = Pg2SqliteOptions::default();
    let base = parse_create_trigger(
        "CREATE TRIGGER trg AFTER INSERT ON users FOR EACH ROW EXECUTE FUNCTION trigger_fn()",
    );

    let ok = base.translate(&schema, &options).expect("translation should succeed");
    assert!(!ok.is_empty(), "expected at least one translated trigger");

    let mut with_drop = base.clone();
    with_drop.or_replace = true;
    let translated = with_drop.translate(&schema, &options).expect("translation should succeed");
    assert!(
        translated.iter().any(|(drop, _)| drop.is_some()),
        "expected at least one DROP TRIGGER emitted for OR REPLACE"
    );

    let missing_body_err =
        base.translate(&schema_without_fn, &options).expect_err("missing function body must error");
    assert!(missing_body_err.to_string().contains("Trigger function"));

    let mut has_statements = base.clone();
    has_statements.statements =
        Some(ConditionalStatements::Sequence { statements: vec![parse_statement("SELECT 1")] });
    assert!(
        unsupported_message(has_statements.translate(&schema, &options).unwrap_err())
            .contains("Triggers with statements")
    );

    let mut no_exec_body = base.clone();
    no_exec_body.exec_body = None;
    assert!(
        unsupported_message(no_exec_body.translate(&schema, &options).unwrap_err())
            .contains("without an execution body")
    );

    let mut procedure = base.clone();
    procedure.exec_body.as_mut().expect("exec body").exec_type = TriggerExecBodyType::Procedure;
    assert!(
        unsupported_message(procedure.translate(&schema, &options).unwrap_err())
            .contains("Procedure")
    );

    let mut or_alter = base.clone();
    or_alter.or_alter = true;
    assert!(
        unsupported_message(or_alter.translate(&schema, &options).unwrap_err())
            .contains("OR ALTER")
    );

    let mut constraint = base.clone();
    constraint.is_constraint = true;
    assert!(
        unsupported_message(constraint.translate(&schema, &options).unwrap_err())
            .contains("Constraint triggers")
    );

    let mut with_characteristics = base;
    with_characteristics.characteristics = Some(ConstraintCharacteristics::default());
    assert!(
        unsupported_message(with_characteristics.translate(&schema, &options).unwrap_err())
            .contains("characteristics")
    );
}

#[test]
fn schema_function_body_covers_missing_and_error_paths() {
    let missing = empty_schema();
    assert!(missing.function_body("does_not_exist").expect("lookup should succeed").is_none());

    let mut no_body_stmt = parse_statement(
        "CREATE FUNCTION no_body() RETURNS TRIGGER AS $$ BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql;",
    );
    if let Statement::CreateFunction(func) = &mut no_body_stmt {
        func.function_body = Some(CreateFunctionBody::AsAfterOptions(Expr::Identifier(
            Ident::new("body_placeholder"),
        )));
    }
    let no_body_schema = ParserDB::from_statements(vec![no_body_stmt], "test".to_string())
        .expect("schema should build");
    assert!(no_body_schema.function_body("no_body").expect("lookup should succeed").is_none());

    let no_begin_schema = schema_from_sql(
        "CREATE FUNCTION no_begin() RETURNS TRIGGER AS $$ SELECT 1; $$ LANGUAGE plpgsql;",
    );
    let err = no_begin_schema.function_body("no_begin").unwrap_err();
    assert!(unsupported_message(err).contains("must contain BEGIN...END block"));

    let no_end_schema = schema_from_sql(
        "CREATE FUNCTION no_end() RETURNS TRIGGER AS $$ BEGIN SELECT 1; $$ LANGUAGE plpgsql;",
    );
    let err = no_end_schema.function_body("no_end").unwrap_err();
    assert!(unsupported_message(err).contains("must end with END"));

    let tokenize_error_schema = schema_from_sql(
        "CREATE FUNCTION bad_tokens() RETURNS TRIGGER AS $$ BEGIN SELECT 'unterminated; END; $$ LANGUAGE plpgsql;",
    );
    let err = tokenize_error_schema.function_body("bad_tokens").unwrap_err();
    assert!(unsupported_message(err).contains("Failed to tokenize trigger function"));

    let parse_error_schema = schema_from_sql(
        "CREATE FUNCTION bad_parse() RETURNS TRIGGER AS $$ BEGIN SELECT FROM; END; $$ LANGUAGE plpgsql;",
    );
    let err = parse_error_schema.function_body("bad_parse").unwrap_err();
    assert!(unsupported_message(err).contains("Failed to parse trigger function"));

    let return_strip_schema = schema_from_sql(
        r#"
        CREATE FUNCTION strip_return() RETURNS TRIGGER AS $$
        BEGIN
            SELECT 1;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        "#,
    );
    let body = return_strip_schema
        .function_body("strip_return")
        .expect("function body should parse")
        .expect("body should exist");
    assert_eq!(body.statements.len(), 1, "RETURN NEW should be stripped");
}

#[test]
fn rls_public_helpers_cover_no_pk_and_readonly_paths() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE docs(id INTEGER PRIMARY KEY, owner TEXT, body TEXT);
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs FOR SELECT USING (owner = 'alice');
        CREATE POLICY docs_insert ON docs FOR INSERT WITH CHECK (owner = 'alice');
        CREATE POLICY docs_update ON docs FOR UPDATE USING (owner = 'alice') WITH CHECK (owner = 'alice');
        CREATE POLICY docs_delete ON docs FOR DELETE USING (owner = 'alice');

        CREATE TABLE logs(msg TEXT);
        ALTER TABLE logs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY logs_select_no_using ON logs FOR SELECT;

        CREATE TABLE public_table(id INTEGER PRIMARY KEY, body TEXT);
        "#,
    );
    let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");

    assert!(table_has_rls("docs", &schema));
    assert!(!table_has_rls("public_table", &schema));

    let docs_table = schema.table(None, "docs").expect("docs table must exist");
    let public_table = schema.table(None, "public_table").expect("public table must exist");

    assert_eq!(resolve_trigger_table_name("docs", docs_table, &schema, &options), "docs_rls");
    assert_eq!(
        resolve_trigger_table_name("public_table", public_table, &schema, &options),
        "public_table"
    );

    let docs_rls_sql = generate_rls_view_sql(docs_table, &schema, &options)
        .expect("sql generation should succeed");
    assert!(docs_rls_sql.contains("WHERE"), "expected policy WHERE clause");

    let logs_table = schema.table(None, "logs").expect("logs table must exist");
    let logs_rls_sql = generate_rls_view_sql(logs_table, &schema, &options)
        .expect("sql generation should succeed");
    assert!(
        !logs_rls_sql.contains(" WHERE "),
        "SELECT policy without USING should produce no WHERE clause"
    );

    let rw = generate_rls_statements(docs_table, &schema, &options).expect("rw rls statements");
    assert!(
        rw.iter().any(|s| matches!(s, Statement::CreateView { .. })),
        "expected generated RLS view"
    );
    assert!(
        rw.iter().any(|s| matches!(s, Statement::CreateTrigger { .. })),
        "expected generated RLS triggers"
    );

    let readonly = generate_readonly_rls_statements(docs_table, &schema, &options)
        .expect("readonly statements");
    let readonly_trigger_count =
        readonly.iter().filter(|s| matches!(s, Statement::CreateTrigger { .. })).count();
    assert!(
        readonly_trigger_count >= 2,
        "read-only RLS still includes validation monitoring triggers"
    );

    let no_pk_validation =
        generate_rls_validation_statements(logs_table, &schema, &options, "rls_audit")
            .expect("validation statements should parse");
    assert!(
        no_pk_validation.iter().any(|s| matches!(s, Statement::CreateView { .. })),
        "expected validation view for no-PK table"
    );
}

#[test]
fn reverse_translation_covers_uncommon_query_variants() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    let mut all_query = parse_query("SELECT ALL 1");
    if let SetExpr::Select(select) = all_query.body.as_mut() {
        select.distinct = Some(sqlparser::ast::Distinct::All);
        select.group_by = sqlparser::ast::GroupByExpr::All(vec![]);
        select.named_window = vec![sqlparser::ast::NamedWindowDefinition(
            Ident::new("w2"),
            sqlparser::ast::NamedWindowExpr::NamedWindow(Ident::new("w1")),
        )];
    }
    all_query.order_by = Some(sqlparser::ast::OrderBy {
        kind: sqlparser::ast::OrderByKind::All(sqlparser::ast::OrderByOptions {
            sort: Some(sqlparser::ast::OrderBySort::Asc),
            nulls_first: Some(false),
        }),
        interpolate: None,
    });
    let translated_query = all_query.reverse_translate(&schema, &options).expect("reverse query");
    assert!(translated_query.order_by.is_some());

    let set_operation = SetExpr::SetOperation {
        op: sqlparser::ast::SetOperator::Union,
        set_quantifier: sqlparser::ast::SetQuantifier::All,
        left: Box::new(SetExpr::Query(Box::new(parse_query("SELECT 1")))),
        right: Box::new(SetExpr::Values(sqlparser::ast::Values {
            explicit_row: false,
            rows: vec![sqlparser::ast::Parens {
                opening_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                content: vec![Expr::Value(sqlparser::ast::ValueWithSpan::from(
                    sqlparser::ast::Value::Number("1".to_string(), false),
                ))],
                closing_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
            }],
            value_keyword: false,
        })),
    };
    let _ = set_operation.reverse_translate(&schema, &options).expect("reverse set operation");

    let passthroughs: Vec<SetExpr> = vec![
        SetExpr::Insert(parse_statement("INSERT INTO t VALUES (1)")),
        SetExpr::Update(parse_statement("UPDATE t SET c = 1")),
        SetExpr::Delete(parse_statement("DELETE FROM t")),
        SetExpr::Merge(parse_statement(
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET c = 1",
        )),
        SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("t".to_string()),
            schema_name: None,
        })),
    ];
    for passthrough in passthroughs {
        let out = passthrough.reverse_translate(&schema, &options).expect("reverse set expr");
        assert_eq!(out, passthrough);
    }
}

#[test]
fn reverse_translation_covers_uncommon_function_variants() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let make_func = |name: &str, args: Vec<FunctionArg>| -> Expr {
        Expr::Function(Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args,
                clauses: vec![],
            }),
            filter: None,
            null_treatment: None,
            over: None,
            within_group: vec![],
            parameters: FunctionArguments::None,
            uses_odbc_syntax: false,
        })
    };

    let bad_instr = make_func("instr", vec![FunctionArg::Unnamed(FunctionArgExpr::Wildcard)]);
    let err = bad_instr.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("INSTR requires exactly 2 arguments"));

    let bad_vec = make_func(
        "vec_distance_l2",
        vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(Ident::new("a"))))],
    );
    let err = bad_vec.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("requires exactly 2 arguments"));

    let bad_vec_f32 = make_func(
        "vec_f32",
        vec![
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(Ident::new("a")))),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(Ident::new("b")))),
        ],
    );
    let err = bad_vec_f32.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("vec_f32 requires exactly 1 argument"));

    let strftime_with_wildcard = make_func(
        "strftime",
        vec![
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::ValueWithSpan::from(sqlparser::ast::Value::SingleQuotedString(
                    "%Y".to_string(),
                )),
            ))),
            FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
        ],
    );
    let err = strftime_with_wildcard.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Invalid strftime arguments"));

    let datetime_prefixed_offset = make_func(
        "datetime",
        vec![
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(Ident::new("created_at")))),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::ValueWithSpan {
                    value: sqlparser::ast::Value::SingleQuotedString("utc+02:30".to_string()),
                    span: Span::empty(),
                },
            ))),
        ],
    );
    let translated = datetime_prefixed_offset
        .reverse_translate(&schema, &options)
        .expect("datetime timezone should reverse");
    assert!(translated.to_string().contains("AT TIME ZONE"));
}

#[test]
fn forward_expr_translation_covers_remaining_fts_extract_and_timezone_paths() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body TEXT, created_at TEXT);
        CREATE TABLE composite_docs(
            id INTEGER,
            tenant_id INTEGER,
            title TEXT,
            PRIMARY KEY(id, tenant_id)
        );
        "#,
    );
    let empty = empty_schema();
    let options = Pg2SqliteOptions::default();

    let no_columns = parse_expr("to_tsvector('english', 'literal') @@ to_tsquery('hello & world')");
    let err = no_columns.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Could not determine table name"));

    let missing_table = parse_expr("to_tsvector(title) @@ to_tsquery('hello')");
    let err = missing_table.translate(&empty, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Could not determine table name"));

    // Composite PK + declared GIN index: the FTS-index gate passes (because the
    // index IS declared in the schema and the full-pipeline catalog will pick
    // it up), and the deeper single-column-PK check fires. Drive through the
    // full pipeline so `populate_fts_index_catalog` populates the catalog.
    let composite_pk_err = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(
            "CREATE TABLE composite_docs(id INTEGER, tenant_id INTEGER, title TEXT, \
             PRIMARY KEY(id, tenant_id)); \
             CREATE INDEX composite_docs_fts ON composite_docs USING GIN (to_tsvector('english', title)); \
             SELECT id FROM composite_docs WHERE to_tsvector('english', title) @@ to_tsquery('hello');",
        )
        .expect("parse")
        .translate(&options)
        .unwrap_err();
    assert!(unsupported_message(composite_pk_err).contains("single-column primary key"));

    // GIN index declared but tsquery argument is a non-literal expression: gate
    // passes, the literal-argument check fires.
    let tsquery_not_literal_err = pg2sqlite::prelude::Pg2Sqlite::default()
        .sql(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT); \
             CREATE INDEX docs_fts ON docs USING GIN (to_tsvector('english', title)); \
             SELECT id FROM docs WHERE to_tsvector('english', title) @@ to_tsquery(search_term);",
        )
        .expect("parse")
        .translate(&options)
        .unwrap_err();
    let tsquery_err_msg = unsupported_message(tsquery_not_literal_err);
    assert!(tsquery_err_msg.contains("to_tsquery"), "unexpected error: {tsquery_err_msg}");

    // No GIN index declared at all: the new FTS-index gate fires with a clear
    // "FTS5 index ... not declared" message before any deeper check runs.
    // This pins the new gate so the silent-passthrough regression cannot
    // come back.
    let no_index_schema = schema_from_sql("CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT);");
    let no_index = parse_expr("to_tsvector(title) @@ to_tsquery('hello')");
    let err = no_index.translate(&no_index_schema, &options).unwrap_err();
    let err_msg = unsupported_message(err);
    assert!(
        err_msg.contains("FTS5 index") && err_msg.contains("not declared"),
        "expected FTS-index gate error, got: {err_msg}"
    );

    let extract_epoch = parse_expr("EXTRACT(EPOCH FROM created_at)");
    let translated_epoch =
        extract_epoch.translate(&schema, &options).expect("EXTRACT(EPOCH) should now be supported");
    assert!(
        translated_epoch.to_string().contains("strftime('%s'"),
        "EXTRACT(EPOCH) should use strftime('%s', ...), got: {translated_epoch}"
    );

    let at_tz_prefixed = parse_expr("created_at AT TIME ZONE 'utc+02:30'");
    let translated =
        at_tz_prefixed.translate(&schema, &options).expect("timezone should translate");
    assert!(translated.to_string().contains("'+02:30'"));

    let at_tz_invalid_offset = parse_expr("created_at AT TIME ZONE '+25:00'");
    let err = at_tz_invalid_offset.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("AT TIME ZONE supports only literal"));

    let at_tz_non_literal = parse_expr("created_at AT TIME ZONE tz_value");
    let err = at_tz_non_literal.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("AT TIME ZONE supports only literal"));
}

#[test]
fn forward_function_translation_covers_named_filter_and_none_argument_paths() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    let named_concat = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("concat"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Named {
                    name: Ident::new("a"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("x".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                },
                FunctionArg::Named {
                    name: Ident::new("b"),
                    arg: FunctionArgExpr::Expr(Expr::Value(sqlparser::ast::ValueWithSpan::from(
                        sqlparser::ast::Value::SingleQuotedString("y".to_string()),
                    ))),
                    operator: FunctionArgOperator::RightArrow,
                },
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let translated =
        named_concat.translate(&schema, &options).expect("named concat should translate");
    assert!(translated.to_string().contains("||"));

    let wildcard_concat = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("concat"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Wildcard)],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let err = wildcard_concat.translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("CONCAT requires at least one argument"));

    let filtered_named = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("sum"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Named {
                name: Ident::new("amount"),
                arg: FunctionArgExpr::Expr(Expr::Identifier(Ident::new("amount"))),
                operator: FunctionArgOperator::Equals,
            }],
            clauses: vec![],
        }),
        filter: Some(Box::new(parse_expr("amount > 0"))),
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let filtered = filtered_named.translate(&schema, &options).expect("FILTER should translate");
    let filtered_sql = filtered.to_string();
    assert!(filtered_sql.contains("CASE WHEN"));
    assert!(!filtered_sql.contains("FILTER"));

    let filtered_no_args = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("count"))]),
        args: FunctionArguments::None,
        filter: Some(Box::new(parse_expr("1 = 1"))),
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let passthrough = filtered_no_args
        .translate(&schema, &options)
        .expect("FILTER with FunctionArguments::None should pass through");
    assert!(passthrough.to_string().contains("count"));
}

#[test]
fn reverse_function_translation_covers_argument_error_and_passthrough_shapes() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    let instr_bad_arg = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("instr"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(Ident::new("name")))),
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let err = instr_bad_arg.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("Expected expression argument in function"));

    let passthrough_none = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("custom_fn"))]),
        args: FunctionArguments::None,
        filter: Some(Box::new(parse_expr("1 = 1"))),
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let translated = passthrough_none
        .reverse_translate(&schema, &options)
        .expect("passthrough function should reverse");
    assert!(translated.to_string().contains("custom_fn"));

    let passthrough_named = Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("custom_fn"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Named {
                    name: Ident::new("x"),
                    arg: FunctionArgExpr::Expr(Expr::Identifier(Ident::new("value"))),
                    operator: FunctionArgOperator::RightArrow,
                },
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let translated = passthrough_named
        .reverse_translate(&schema, &options)
        .expect("named-arg passthrough function should reverse");
    let translated_sql = translated.to_string();
    assert!(translated_sql.contains("x => value"));
    assert!(translated_sql.contains('*'));
}

#[test]
fn reverse_insert_translation_covers_replace_table_function_and_partitioned_paths() {
    let schema = schema_from_sql("CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT);");
    let options = Pg2SqliteOptions::default();

    let mut replace_with_table_fn =
        parse_insert("INSERT OR REPLACE INTO docs (id, title) VALUES (1, 'a')");
    replace_with_table_fn.table = TableObject::TableFunction(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("remote_docs"))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
        uses_odbc_syntax: false,
    });
    let err = replace_with_table_fn.reverse_translate(&schema, &options).unwrap_err();
    assert!(unsupported_message(err).contains("table function is not supported"));

    let mut with_partitioned = parse_insert("INSERT INTO docs (id, title) VALUES (1, 'a')");
    with_partitioned.partitioned = Some(vec![parse_expr("id + 1")]);
    let reversed = with_partitioned
        .reverse_translate(&schema, &options)
        .expect("insert with partitioned expressions should reverse");
    assert!(reversed.partitioned.is_some());

    let with_conflict = parse_insert(
        "INSERT INTO docs (id, title) VALUES (1, 'a')
         ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title WHERE id > 0",
    );
    let reversed =
        with_conflict.reverse_translate(&schema, &options).expect("upsert should reverse");
    assert!(reversed.on.is_some());

    let fail_insert = parse_insert("INSERT OR FAIL INTO docs (id, title) VALUES (1, 'a')");
    let reversed =
        fail_insert.reverse_translate(&schema, &options).expect("or fail should reverse");
    assert!(reversed.on.is_none());
}

#[test]
fn rls_view_generation_covers_subquery_join_transform_paths() {
    let schema = schema_from_sql(
        r#"
        CREATE TABLE docs(
            id INTEGER PRIMARY KEY,
            owner_id INTEGER NOT NULL,
            team_id INTEGER NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs
            FOR SELECT
            USING (
                EXISTS (
                    SELECT d.owner_id
                    FROM (SELECT owner_id, team_id FROM docs) AS d
                    JOIN (teams t JOIN memberships m ON t.id = m.team_id)
                      ON m.team_id = d.team_id
                    WHERE d.owner_id = docs.owner_id
                    GROUP BY d.owner_id
                    HAVING d.owner_id IS NOT NULL
                )
            );
        CREATE TABLE teams(id INTEGER PRIMARY KEY, team_name TEXT);
        CREATE TABLE memberships(id INTEGER PRIMARY KEY, team_id INTEGER, user_id INTEGER);
        "#,
    );
    let options = Pg2SqliteOptions::default();
    let docs = schema.table(None, "docs").expect("docs table should exist");

    let sql = generate_rls_view_sql(docs, &schema, &options).expect("RLS view SQL should generate");
    assert!(sql.contains("docs_rls"), "expected backing table rename in SQL: {sql}");
    assert!(sql.contains("EXISTS"), "expected EXISTS policy to be preserved: {sql}");
    assert!(sql.contains("JOIN"), "expected JOIN subquery rewrite path: {sql}");
}
