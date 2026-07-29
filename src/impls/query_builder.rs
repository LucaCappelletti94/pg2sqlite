//! Constructors for the [`Query`] / [`Select`] / [`TableFactor`] shapes the
//! translators synthesize.
//!
//! `sqlparser` models a `SELECT` as a struct with a couple of dozen
//! dialect-specific fields, almost all of which a translated statement leaves
//! at their neutral value. Building those literals inline buries the three
//! fields that matter, and every new upstream field breaks each copy, so the
//! neutral shape lives here once.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{boxed::Box, vec, vec::Vec};

use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, GroupByExpr, Ident, ObjectName, ObjectNamePart, Query,
    Select, SelectFlavor, SelectItem, SetExpr, TableAlias, TableFactor, TableFunctionArgs,
    TableWithJoins, With, helpers::attached_token::AttachedToken,
};

/// Create a minimal [`Query`] from a WITH clause and body, with all other
/// fields set to their defaults (no ORDER BY, no LIMIT, no FETCH, etc.).
#[must_use]
pub(crate) fn make_query(with: Option<With>, body: SetExpr) -> Query {
    Query {
        with,
        body: Box::new(body),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: vec![],
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: vec![],
    }
}

/// Create a minimal [`Select`] with projection, FROM clause, and optional
/// WHERE clause. All other fields are set to defaults.
#[must_use]
pub(crate) fn make_simple_select(
    projection: Vec<SelectItem>,
    from: Vec<TableWithJoins>,
    selection: Option<Expr>,
) -> Select {
    Select {
        select_token: AttachedToken::empty(),
        distinct: None,
        top: None,
        top_before_distinct: false,
        projection,
        into: None,
        from,
        lateral_views: vec![],
        selection,
        group_by: GroupByExpr::Expressions(vec![], vec![]),
        cluster_by: vec![],
        distribute_by: vec![],
        sort_by: vec![],
        having: None,
        named_window: vec![],
        qualify: None,
        window_before_qualify: false,
        value_table_mode: None,
        connect_by: vec![],
        flavor: SelectFlavor::Standard,
        exclude: None,
        optimizer_hints: Vec::new(),
        prewhere: None,
        select_modifiers: None,
    }
}

/// Wrap a single-expression projection over `from` into a [`Query`], the shape
/// every scalar subquery and `EXISTS` body in the translators uses.
#[must_use]
pub(crate) fn single_expr_query(
    projection: Expr,
    from: Vec<TableWithJoins>,
    selection: Option<Expr>,
) -> Query {
    make_query(
        None,
        SetExpr::Select(Box::new(make_simple_select(
            vec![SelectItem::UnnamedExpr(projection)],
            from,
            selection,
        ))),
    )
}

/// A bare `FROM <name>` relation with no alias, arguments, or hints.
#[must_use]
pub(crate) fn plain_table_factor(name: ObjectName) -> TableFactor {
    TableFactor::Table {
        name,
        alias: None,
        args: None,
        with_hints: Vec::new(),
        version: None,
        with_ordinality: false,
        partitions: Vec::new(),
        json_path: None,
        sample: None,
        index_hints: Vec::new(),
    }
}

/// A table-valued function relation, `FROM <name>(<args>)`, as SQLite spells
/// `json_each(x)`.
#[must_use]
pub(crate) fn table_function_factor(
    name: &str,
    args: Vec<Expr>,
    alias: Option<TableAlias>,
    with_ordinality: bool,
) -> TableFactor {
    TableFactor::Table {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        alias,
        args: Some(TableFunctionArgs {
            args: args
                .into_iter()
                .map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e)))
                .collect(),
            settings: None,
        }),
        with_hints: Vec::new(),
        version: None,
        with_ordinality,
        partitions: Vec::new(),
        json_path: None,
        sample: None,
        index_hints: Vec::new(),
    }
}

/// Wrap a relation as a join-free `FROM` item.
#[must_use]
pub(crate) fn from_relation(relation: TableFactor) -> Vec<TableWithJoins> {
    vec![TableWithJoins { relation, joins: Vec::new() }]
}
