use alloc::{boxed::Box, vec, vec::Vec};

use sqlparser::{
    ast::{
        Assignment, BeginEndStatements, ColumnDef, ConditionalStatements, CreateTable,
        CreateTableOptions, CreateTrigger, CreateView, Delete, Expr, FromTable,
        HiveDistributionStyle, Ident, Insert, ObjectName, Query, SelectItem, Statement,
        TableObject, TriggerEvent, TriggerObject, TriggerObjectKind, TriggerPeriod, Update, Values,
        ViewColumnDef, helpers::attached_token::AttachedToken,
    },
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan, Word},
};

use super::{
    function_helpers::{simple_function_expr, string_literal},
    query_builder::{from_relation, make_query, plain_table_factor, single_expr_query},
};

fn keyword_token(value: &str, keyword: Keyword) -> AttachedToken {
    AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
        value: value.into(),
        quote_style: None,
        keyword,
    })))
}

pub(crate) fn trigger(
    name: ObjectName,
    table_name: ObjectName,
    period: TriggerPeriod,
    event: TriggerEvent,
    for_each_row: bool,
    condition: Option<Expr>,
    statements: Vec<Statement>,
) -> Statement {
    Statement::CreateTrigger(CreateTrigger {
        or_alter: false,
        temporary: false,
        or_replace: false,
        is_constraint: false,
        name,
        period: Some(period),
        period_before_table: true,
        events: vec![event],
        table_name,
        referenced_table_name: None,
        referencing: Vec::new(),
        trigger_object: for_each_row.then_some(TriggerObjectKind::ForEach(TriggerObject::Row)),
        condition,
        exec_body: None,
        statements_as: false,
        statements: Some(ConditionalStatements::BeginEnd(BeginEndStatements {
            begin_token: keyword_token("BEGIN", Keyword::BEGIN),
            statements,
            end_token: keyword_token("END", Keyword::END),
        })),
        characteristics: None,
    })
}

pub(crate) fn insert(table: ObjectName, columns: Vec<ObjectName>, source: Query) -> Statement {
    Statement::Insert(Insert {
        insert_token: AttachedToken::empty(),
        optimizer_hints: Vec::new(),
        or: None,
        ignore: false,
        into: true,
        table: TableObject::TableName(table),
        table_alias: None,
        columns,
        overwrite: false,
        source: Some(Box::new(source)),
        assignments: Vec::new(),
        partitioned: None,
        after_columns: Vec::new(),
        has_table_keyword: false,
        on: None,
        returning: None,
        output: None,
        replace_into: false,
        priority: None,
        insert_alias: None,
        settings: None,
        format_clause: None,
        multi_table_insert_type: None,
        multi_table_into_clauses: Vec::new(),
        multi_table_when_clauses: Vec::new(),
        multi_table_else_clause: None,
    })
}

pub(crate) fn values(rows: Vec<Vec<Expr>>) -> Query {
    make_query(
        None,
        sqlparser::ast::SetExpr::Values(Values {
            explicit_row: false,
            value_keyword: false,
            rows: rows.into_iter().map(sqlparser::ast::Parens::with_empty_span).collect(),
        }),
    )
}

pub(crate) fn delete(table: ObjectName, selection: Option<Expr>) -> Statement {
    Statement::Delete(Delete {
        delete_token: AttachedToken::empty(),
        optimizer_hints: Vec::new(),
        tables: Vec::new(),
        from: FromTable::WithFromKeyword(from_relation(plain_table_factor(table))),
        using: None,
        selection,
        returning: None,
        output: None,
        order_by: Vec::new(),
        limit: None,
    })
}
pub(crate) fn update(
    table: ObjectName,
    assignments: Vec<Assignment>,
    selection: Option<Expr>,
) -> Statement {
    Statement::Update(Update {
        update_token: AttachedToken::empty(),
        optimizer_hints: Vec::new(),
        table: from_relation(plain_table_factor(table))
            .pop()
            .expect("a table relation contains one item"),
        assignments,
        from: None,
        selection,
        returning: None,
        output: None,
        or: None,
        order_by: Vec::new(),
        limit: None,
    })
}
pub(crate) fn create_table(
    name: ObjectName,
    columns: Vec<ColumnDef>,
    if_not_exists: bool,
    strict: bool,
) -> Statement {
    Statement::CreateTable(CreateTable {
        name,
        columns,
        constraints: Vec::new(),
        if_not_exists,
        strict,
        or_replace: false,
        temporary: false,
        unlogged: false,
        external: false,
        dynamic: false,
        global: None,
        transient: false,
        volatile: false,
        iceberg: false,
        snapshot: false,
        hive_distribution: HiveDistributionStyle::NONE,
        hive_formats: None,
        table_options: CreateTableOptions::None,
        file_format: None,
        location: None,
        query: None,
        without_rowid: false,
        like: None,
        clone: None,
        version: None,
        comment: None,
        on_commit: None,
        on_cluster: None,
        primary_key: None,
        order_by: None,
        partition_by: None,
        cluster_by: None,
        clustered_by: None,
        inherits: None,
        partition_of: None,
        for_values: None,
        copy_grants: false,
        enable_schema_evolution: None,
        change_tracking: None,
        data_retention_time_in_days: None,
        max_data_extension_time_in_days: None,
        default_ddl_collation: None,
        with_aggregation_policy: None,
        with_row_access_policy: None,
        with_storage_lifecycle_policy: None,
        with_tags: None,
        external_volume: None,
        with_connection: None,
        base_location: None,
        catalog: None,
        catalog_sync: None,
        storage_serialization_policy: None,
        target_lag: None,
        warehouse: None,
        refresh_mode: None,
        initialize: None,
        require_user: false,
        diststyle: None,
        distkey: None,
        sortkey: None,
        backup: None,
        multiset: None,
        fallback: None,
        with_data: None,
    })
}

pub(crate) fn create_view(
    name: ObjectName,
    columns: Vec<ViewColumnDef>,
    query: Query,
) -> Statement {
    Statement::CreateView(CreateView {
        or_alter: false,
        or_replace: false,
        materialized: false,
        secure: false,
        name,
        name_before_not_exists: false,
        columns,
        query: Box::new(query),
        options: CreateTableOptions::None,
        cluster_by: Vec::new(),
        comment: None,
        with_no_schema_binding: false,
        if_not_exists: false,
        temporary: false,
        copy_grants: false,
        to: None,
        params: None,
    })
}

pub(crate) fn select_statement(query: Query) -> Statement {
    Statement::Query(Box::new(query))
}
pub(crate) fn select_expression_statement(expression: Expr, selection: Option<Expr>) -> Statement {
    select_statement(single_expr_query(expression, Vec::new(), selection))
}

pub(crate) fn raise_statement(
    action: &str,
    message: Option<&str>,
    selection: Option<Expr>,
) -> Statement {
    let arguments = core::iter::once(Expr::Identifier(Ident::new(action)))
        .chain(message.map(string_literal))
        .collect();
    select_expression_statement(simple_function_expr("RAISE", arguments, None), selection)
}

pub(crate) fn select_items(expressions: Vec<Expr>) -> Vec<SelectItem> {
    expressions.into_iter().map(SelectItem::UnnamedExpr).collect()
}
