//! Implementation of the [`ReverseTranslator`] trait for the
//! `Statement` type.

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    AccessExpr, Delete, Expr, ExprWithAlias, FromTable, FunctionArg, FunctionArgExpr,
    FunctionArguments, GroupByExpr, Insert, JoinConstraint, JoinOperator, JsonPathElem,
    JsonTableColumn, LimitClause, Measure, ObjectName, OrderByExpr, PivotValueSource, Query,
    Select, SelectItem, SetExpr, Setting, Statement, Subscript, SymbolDefinition, Table,
    TableFactor, TableFunctionArgs, TableObject, TableSample, TableSampleBucket, TableSampleKind,
    TableSampleQuantity, TableVersion, TableWithJoins, Update, UpdateTableFromKind, Values,
    WindowType, WithFill, XmlNamespaceDefinition, XmlPassingArgument, XmlPassingClause,
    XmlTableColumn, XmlTableColumnOption,
};

use crate::{
    errors::Error,
    impls::{object_name::last_ident, shared_helpers::statement_variant_name},
    prelude::{Pg2SqliteOptions, ReverseTranslator},
    traits::TranslationOptions,
};

/// Check if a table name ends with the RLS suffix.
fn strip_identifier_quotes(name: &str) -> &str {
    if name.len() >= 2 {
        let first = name.as_bytes()[0] as char;
        let last = name.as_bytes()[name.len() - 1] as char;
        if matches!((first, last), ('"', '"') | ('`', '`') | ('[', ']')) {
            return &name[1..name.len() - 1];
        }
    }
    name
}

/// Checks whether an identifier string ends with a suffix, ignoring outer
/// quotes.
fn identifier_has_suffix(name: &str, suffix: &str) -> bool {
    strip_identifier_quotes(name).ends_with(suffix)
}

/// Check if a table name ends with the RLS suffix.
fn is_rls_table(name: &ObjectName, options: &Pg2SqliteOptions) -> bool {
    let suffix = options.get_rls_table_suffix();
    last_ident(name).is_some_and(|ident| ident.value.ends_with(suffix))
}

/// Check a table reference for RLS table access.
fn check_table_for_rls(name: &ObjectName, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if is_rls_table(name, options) {
        return Err(Error::RlsTableDetected {
            table_name: name.to_string(),
            suffix: options.get_rls_table_suffix().to_string(),
        });
    }
    Ok(())
}

/// Check a TableObject for RLS table access.
fn check_table_object_for_rls(
    table: &TableObject,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match table {
        TableObject::TableName(name) => check_table_for_rls(name, options),
        TableObject::TableFunction(_) => Ok(()),
    }
}

fn check_table_command_for_rls(table: &Table, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(table_name) = &table.table_name {
        let full_name = table
            .schema_name
            .as_ref()
            .map_or_else(|| table_name.clone(), |schema| format!("{schema}.{table_name}"));
        let suffix = options.get_rls_table_suffix();
        if identifier_has_suffix(table_name, suffix) {
            return Err(Error::RlsTableDetected {
                table_name: full_name,
                suffix: suffix.to_string(),
            });
        }
    }
    Ok(())
}

fn check_function_arg_expr_for_rls(
    arg: &FunctionArgExpr,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match arg {
        FunctionArgExpr::Expr(expr) => check_expr_for_rls(expr, options),
        FunctionArgExpr::QualifiedWildcard(_) | FunctionArgExpr::Wildcard => Ok(()),
    }
}

fn check_function_arg_for_rls(arg: &FunctionArg, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match arg {
        FunctionArg::Named { arg, .. }
        | FunctionArg::ExprNamed { arg, .. }
        | FunctionArg::Unnamed(arg) => check_function_arg_expr_for_rls(arg, options),
    }
}

fn check_setting_for_rls(setting: &Setting, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_expr_for_rls(&setting.value, options)
}

fn check_table_function_args_for_rls(
    args: &TableFunctionArgs,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for arg in &args.args {
        check_function_arg_for_rls(arg, options)?;
    }
    if let Some(settings) = &args.settings {
        for setting in settings {
            check_setting_for_rls(setting, options)?;
        }
    }
    Ok(())
}

fn check_table_version_for_rls(
    version: &TableVersion,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match version {
        TableVersion::ForSystemTimeAsOf(expr)
        | TableVersion::TimestampAsOf(expr)
        | TableVersion::VersionAsOf(expr)
        | TableVersion::Function(expr) => check_expr_for_rls(expr, options),
    }
}

fn check_table_sample_quantity_for_rls(
    quantity: &TableSampleQuantity,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&quantity.value, options)
}

fn check_table_sample_bucket_for_rls(
    bucket: &TableSampleBucket,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let Some(on) = &bucket.on {
        check_expr_for_rls(on, options)?;
    }
    Ok(())
}

fn check_table_sample_for_rls(
    sample: &TableSample,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let Some(quantity) = &sample.quantity {
        check_table_sample_quantity_for_rls(quantity, options)?;
    }
    if let Some(bucket) = &sample.bucket {
        check_table_sample_bucket_for_rls(bucket, options)?;
    }
    if let Some(offset) = &sample.offset {
        check_expr_for_rls(offset, options)?;
    }
    Ok(())
}

fn check_table_sample_kind_for_rls(
    sample: &TableSampleKind,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match sample {
        TableSampleKind::BeforeTableAlias(sample) | TableSampleKind::AfterTableAlias(sample) => {
            check_table_sample_for_rls(sample, options)
        }
    }
}

fn check_with_fill_for_rls(with_fill: &WithFill, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(from) = &with_fill.from {
        check_expr_for_rls(from, options)?;
    }
    if let Some(to) = &with_fill.to {
        check_expr_for_rls(to, options)?;
    }
    if let Some(step) = &with_fill.step {
        check_expr_for_rls(step, options)?;
    }
    Ok(())
}

fn check_order_by_expr_for_rls(
    order_by_expr: &OrderByExpr,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&order_by_expr.expr, options)?;
    if let Some(with_fill) = &order_by_expr.with_fill {
        check_with_fill_for_rls(with_fill, options)?;
    }
    Ok(())
}

fn check_expr_with_alias_for_rls(
    expr_with_alias: &ExprWithAlias,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&expr_with_alias.expr, options)
}

fn check_pivot_value_source_for_rls(
    value_source: &PivotValueSource,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match value_source {
        PivotValueSource::List(values) => {
            for value in values {
                check_expr_with_alias_for_rls(value, options)?;
            }
            Ok(())
        }
        PivotValueSource::Any(order_by) => {
            for order_by_expr in order_by {
                check_order_by_expr_for_rls(order_by_expr, options)?;
            }
            Ok(())
        }
        PivotValueSource::Subquery(query) => check_query_for_rls(query, options),
    }
}

fn check_measure_for_rls(measure: &Measure, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_expr_for_rls(&measure.expr, options)
}

fn check_symbol_definition_for_rls(
    symbol: &SymbolDefinition,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&symbol.definition, options)
}

#[allow(clippy::only_used_in_recursion)]
fn check_json_table_column_for_rls(
    column: &JsonTableColumn,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match column {
        JsonTableColumn::Named(_) | JsonTableColumn::ForOrdinality(_) => Ok(()),
        JsonTableColumn::Nested(nested) => {
            for column in &nested.columns {
                check_json_table_column_for_rls(column, options)?;
            }
            Ok(())
        }
    }
}

fn check_xml_passing_argument_for_rls(
    argument: &XmlPassingArgument,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&argument.expr, options)
}

fn check_xml_passing_clause_for_rls(
    passing: &XmlPassingClause,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for argument in &passing.arguments {
        check_xml_passing_argument_for_rls(argument, options)?;
    }
    Ok(())
}

fn check_xml_table_column_option_for_rls(
    option: &XmlTableColumnOption,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match option {
        XmlTableColumnOption::NamedInfo { path, default, .. } => {
            if let Some(path) = path {
                check_expr_for_rls(path, options)?;
            }
            if let Some(default) = default {
                check_expr_for_rls(default, options)?;
            }
            Ok(())
        }
        XmlTableColumnOption::ForOrdinality => Ok(()),
    }
}

fn check_xml_table_column_for_rls(
    column: &XmlTableColumn,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_xml_table_column_option_for_rls(&column.option, options)
}

fn check_xml_namespace_definition_for_rls(
    namespace: &XmlNamespaceDefinition,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(&namespace.uri, options)
}

fn check_join_constraint_for_rls(
    constraint: &JoinConstraint,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match constraint {
        JoinConstraint::On(expr) => check_expr_for_rls(expr, options),
        JoinConstraint::Using(_) | JoinConstraint::Natural | JoinConstraint::None => Ok(()),
    }
}

fn check_join_operator_for_rls(
    operator: &JoinOperator,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::CrossJoin(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::RightAnti(constraint)
        | JoinOperator::StraightJoin(constraint) => {
            check_join_constraint_for_rls(constraint, options)
        }
        JoinOperator::AsOf { constraint, match_condition } => {
            check_join_constraint_for_rls(constraint, options)?;
            check_expr_for_rls(match_condition, options)
        }
        JoinOperator::CrossApply | JoinOperator::OuterApply => Ok(()),
    }
}

fn check_table_with_joins_for_rls(
    table_with_joins: &TableWithJoins,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_table_factor_for_rls(&table_with_joins.relation, options)?;
    for join in &table_with_joins.joins {
        check_table_factor_for_rls(&join.relation, options)?;
        check_join_operator_for_rls(&join.join_operator, options)?;
    }
    Ok(())
}

/// Check all table references in a FROM clause for RLS tables.
fn check_from_clause_for_rls(
    from: &[TableWithJoins],
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for table_with_joins in from {
        check_table_with_joins_for_rls(table_with_joins, options)?;
    }
    Ok(())
}

/// Check a FromTable enum for RLS tables.
fn check_from_table_for_rls(from: &FromTable, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
            check_from_clause_for_rls(tables, options)
        }
    }
}

/// Check an UpdateTableFromKind for RLS tables.
fn check_update_from_for_rls(
    from: &UpdateTableFromKind,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match from {
        UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables) => {
            check_from_clause_for_rls(tables, options)
        }
    }
}

/// Check a table factor for RLS table access.
#[allow(clippy::too_many_lines)]
fn check_table_factor_for_rls(
    factor: &TableFactor,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match factor {
        TableFactor::Table { name, args, with_hints, version, sample, .. } => {
            check_table_for_rls(name, options)?;
            if let Some(args) = args {
                check_table_function_args_for_rls(args, options)?;
            }
            for hint in with_hints {
                check_expr_for_rls(hint, options)?;
            }
            if let Some(version) = version {
                check_table_version_for_rls(version, options)?;
            }
            if let Some(sample) = sample {
                check_table_sample_kind_for_rls(sample, options)?;
            }
            Ok(())
        }
        TableFactor::Derived { subquery, sample, .. } => {
            check_query_for_rls(subquery, options)?;
            if let Some(sample) = sample {
                check_table_sample_kind_for_rls(sample, options)?;
            }
            Ok(())
        }
        TableFactor::TableFunction { expr, .. } => check_expr_for_rls(expr, options),
        TableFactor::Function { args, .. } => {
            for arg in args {
                check_function_arg_for_rls(arg, options)?;
            }
            Ok(())
        }
        TableFactor::UNNEST { array_exprs, .. } => check_expr_slice_for_rls(array_exprs, options),
        TableFactor::JsonTable { json_expr, columns, .. } => {
            check_expr_for_rls(json_expr, options)?;
            for column in columns {
                check_json_table_column_for_rls(column, options)?;
            }
            Ok(())
        }
        TableFactor::OpenJsonTable { json_expr, .. } => check_expr_for_rls(json_expr, options),
        TableFactor::NestedJoin { table_with_joins, .. } => {
            check_table_with_joins_for_rls(table_with_joins, options)
        }
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            ..
        } => {
            check_table_factor_for_rls(table, options)?;
            for expr_with_alias in aggregate_functions {
                check_expr_with_alias_for_rls(expr_with_alias, options)?;
            }
            check_expr_slice_for_rls(value_column, options)?;
            check_pivot_value_source_for_rls(value_source, options)?;
            if let Some(default_on_null) = default_on_null {
                check_expr_for_rls(default_on_null, options)?;
            }
            Ok(())
        }
        TableFactor::Unpivot { table, value, columns, .. } => {
            check_table_factor_for_rls(table, options)?;
            check_expr_for_rls(value, options)?;
            for expr_with_alias in columns {
                check_expr_with_alias_for_rls(expr_with_alias, options)?;
            }
            Ok(())
        }
        TableFactor::MatchRecognize {
            table, partition_by, order_by, measures, symbols, ..
        } => {
            check_table_factor_for_rls(table, options)?;
            check_expr_slice_for_rls(partition_by, options)?;
            for order_by_expr in order_by {
                check_order_by_expr_for_rls(order_by_expr, options)?;
            }
            for measure in measures {
                check_measure_for_rls(measure, options)?;
            }
            for symbol in symbols {
                check_symbol_definition_for_rls(symbol, options)?;
            }
            Ok(())
        }
        TableFactor::XmlTable { namespaces, row_expression, passing, columns, .. } => {
            for namespace in namespaces {
                check_xml_namespace_definition_for_rls(namespace, options)?;
            }
            check_expr_for_rls(row_expression, options)?;
            check_xml_passing_clause_for_rls(passing, options)?;
            for column in columns {
                check_xml_table_column_for_rls(column, options)?;
            }
            Ok(())
        }
        TableFactor::SemanticView { dimensions, metrics, facts, where_clause, .. } => {
            check_expr_slice_for_rls(dimensions, options)?;
            check_expr_slice_for_rls(metrics, options)?;
            check_expr_slice_for_rls(facts, options)?;
            if let Some(where_clause) = where_clause {
                check_expr_for_rls(where_clause, options)?;
            }
            Ok(())
        }
    }
}

fn check_expr_pair_for_rls(
    left: &Expr,
    right: &Expr,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(left, options)?;
    check_expr_for_rls(right, options)
}

fn check_expr_slice_for_rls(exprs: &[Expr], options: &Pg2SqliteOptions) -> Result<(), Error> {
    for expr in exprs {
        check_expr_for_rls(expr, options)?;
    }
    Ok(())
}

fn check_case_expr_for_rls(
    operand: Option<&Expr>,
    conditions: &[sqlparser::ast::CaseWhen],
    else_result: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let Some(operand) = operand {
        check_expr_for_rls(operand, options)?;
    }
    for condition in conditions {
        check_expr_pair_for_rls(&condition.condition, &condition.result, options)?;
    }
    if let Some(else_result) = else_result {
        check_expr_for_rls(else_result, options)?;
    }
    Ok(())
}

fn check_trim_expr_for_rls(
    expr: &Expr,
    trim_what: Option<&Expr>,
    trim_characters: Option<&[Expr]>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    if let Some(trim_what) = trim_what {
        check_expr_for_rls(trim_what, options)?;
    }
    if let Some(trim_characters) = trim_characters {
        check_expr_slice_for_rls(trim_characters, options)?;
    }
    Ok(())
}

fn check_substring_expr_for_rls(
    expr: &Expr,
    substring_from: Option<&Expr>,
    substring_for: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    if let Some(substring_from) = substring_from {
        check_expr_for_rls(substring_from, options)?;
    }
    if let Some(substring_for) = substring_for {
        check_expr_for_rls(substring_for, options)?;
    }
    Ok(())
}

fn check_overlay_expr_for_rls(
    expr: &Expr,
    overlay_what: &Expr,
    overlay_from: &Expr,
    overlay_for: Option<&Expr>,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(expr, options)?;
    check_expr_for_rls(overlay_what, options)?;
    check_expr_for_rls(overlay_from, options)?;
    if let Some(overlay_for) = overlay_for {
        check_expr_for_rls(overlay_for, options)?;
    }
    Ok(())
}

fn check_compound_access_for_rls(
    root: &Expr,
    access_chain: &[AccessExpr],
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    check_expr_for_rls(root, options)?;
    for access in access_chain {
        check_access_expr_for_rls(access, options)?;
    }
    Ok(())
}

fn check_access_expr_for_rls(access: &AccessExpr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match access {
        AccessExpr::Dot(expr) => check_expr_for_rls(expr, options),
        AccessExpr::Subscript(subscript) => {
            match subscript {
                Subscript::Index { index } => check_expr_for_rls(index, options),
                Subscript::Slice { lower_bound, upper_bound, stride } => {
                    if let Some(lower_bound) = lower_bound {
                        check_expr_for_rls(lower_bound, options)?;
                    }
                    if let Some(upper_bound) = upper_bound {
                        check_expr_for_rls(upper_bound, options)?;
                    }
                    if let Some(stride) = stride {
                        check_expr_for_rls(stride, options)?;
                    }
                    Ok(())
                }
            }
        }
    }
}

fn check_json_path_for_rls(
    path: &sqlparser::ast::JsonPath,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    for elem in &path.path {
        if let JsonPathElem::Bracket { key } = elem {
            check_expr_for_rls(key, options)?;
        }
    }
    Ok(())
}

/// Check an expression tree for RLS table references in subqueries.
#[allow(clippy::too_many_lines)]
fn check_expr_for_rls(expr: &Expr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match expr {
        Expr::Subquery(query) => check_query_for_rls(query, options),
        Expr::Exists { subquery, .. } => check_query_for_rls(subquery, options),
        Expr::InSubquery { expr, subquery, .. } => {
            check_expr_for_rls(expr, options)?;
            check_query_for_rls(subquery, options)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::AnyOp { left, right, .. }
        | Expr::AllOp { left, right, .. } => check_expr_pair_for_rls(left, right, options),
        Expr::Convert { expr, styles, .. } => {
            check_expr_for_rls(expr, options)?;
            for style in styles {
                check_expr_for_rls(style, options)?;
            }
            Ok(())
        }
        Expr::Function(func) => check_function_for_rls(func, options),
        Expr::UnaryOp { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::Extract { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. }
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::IsTrue(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotFalse(expr)
        | Expr::IsNormalized { expr, .. }
        | Expr::Named { expr, .. } => check_expr_for_rls(expr, options),
        Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) => {
            check_expr_pair_for_rls(left, right, options)
        }
        Expr::Nested(inner) | Expr::OuterJoin(inner) | Expr::Prior(inner) => {
            check_expr_for_rls(inner, options)
        }
        Expr::AtTimeZone { timestamp, time_zone } => {
            check_expr_pair_for_rls(timestamp, time_zone, options)
        }
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => check_expr_pair_for_rls(expr, pattern, options),
        Expr::Tuple(exprs) => check_expr_slice_for_rls(exprs, options),
        Expr::Array(array) => check_expr_slice_for_rls(&array.elem, options),
        Expr::Case { operand, conditions, else_result, .. } => {
            check_case_expr_for_rls(operand.as_deref(), conditions, else_result.as_deref(), options)
        }
        Expr::Between { expr, low, high, .. } => {
            check_expr_for_rls(expr, options)?;
            check_expr_pair_for_rls(low, high, options)
        }
        Expr::InList { expr, list, .. } => {
            check_expr_for_rls(expr, options)?;
            check_expr_slice_for_rls(list, options)
        }
        Expr::Trim { expr, trim_what, trim_characters, .. } => {
            check_trim_expr_for_rls(expr, trim_what.as_deref(), trim_characters.as_deref(), options)
        }
        Expr::Position { expr, r#in } => check_expr_pair_for_rls(expr, r#in, options),
        Expr::Substring { expr, substring_from, substring_for, .. } => {
            check_substring_expr_for_rls(
                expr,
                substring_from.as_deref(),
                substring_for.as_deref(),
                options,
            )
        }
        Expr::Overlay { expr, overlay_what, overlay_from, overlay_for } => {
            check_overlay_expr_for_rls(
                expr,
                overlay_what,
                overlay_from,
                overlay_for.as_deref(),
                options,
            )
        }
        Expr::Prefixed { value, .. } | Expr::Collate { expr: value, .. } => {
            check_expr_for_rls(value, options)
        }
        Expr::Interval(interval) => check_expr_for_rls(&interval.value, options),
        Expr::JsonAccess { value, path } => {
            check_expr_for_rls(value, options)?;
            check_json_path_for_rls(path, options)
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            check_compound_access_for_rls(root, access_chain, options)
        }
        Expr::InUnnest { expr, array_expr, .. } => {
            check_expr_pair_for_rls(expr, array_expr, options)
        }
        Expr::Struct { values, .. } => check_expr_slice_for_rls(values, options),
        Expr::Dictionary(fields) => {
            for field in fields {
                check_expr_for_rls(&field.value, options)?;
            }
            Ok(())
        }
        Expr::Map(map) => {
            for entry in &map.entries {
                check_expr_pair_for_rls(&entry.key, &entry.value, options)?;
            }
            Ok(())
        }
        Expr::Lambda(lambda) => check_expr_for_rls(&lambda.body, options),
        Expr::MemberOf(member) => check_expr_pair_for_rls(&member.value, &member.array, options),
        _ => Ok(()),
    }
}

/// Check a select item for RLS table references in subqueries.
fn check_select_item_for_rls(item: &SelectItem, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
            check_expr_for_rls(expr, options)
        }
        _ => Ok(()),
    }
}

fn check_select_for_rls(select: &Select, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_from_clause_for_rls(&select.from, options)?;

    if let Some(selection) = &select.selection {
        check_expr_for_rls(selection, options)?;
    }

    if let Some(having) = &select.having {
        check_expr_for_rls(having, options)?;
    }

    for item in &select.projection {
        check_select_item_for_rls(item, options)?;
    }

    if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            check_expr_for_rls(expr, options)?;
        }
    }

    if let Some(qualify) = &select.qualify {
        check_expr_for_rls(qualify, options)?;
    }

    Ok(())
}

fn check_values_for_rls(values: &Values, options: &Pg2SqliteOptions) -> Result<(), Error> {
    for row in &values.rows {
        for expr in row {
            check_expr_for_rls(expr, options)?;
        }
    }
    Ok(())
}

fn check_set_expr_for_rls(set_expr: &SetExpr, options: &Pg2SqliteOptions) -> Result<(), Error> {
    match set_expr {
        SetExpr::Select(select) => check_select_for_rls(select, options),
        SetExpr::Query(query) => check_query_for_rls(query, options),
        SetExpr::SetOperation { left, right, .. } => {
            check_set_expr_for_rls(left, options)?;
            check_set_expr_for_rls(right, options)
        }
        SetExpr::Insert(stmt) => {
            if let Statement::Insert(insert) = stmt {
                check_insert_for_rls(insert, options)?;
            }
            Ok(())
        }
        SetExpr::Update(stmt) => {
            if let Statement::Update(update) = stmt {
                check_update_for_rls(update, options)?;
            }
            Ok(())
        }
        SetExpr::Delete(stmt) => {
            if let Statement::Delete(delete) = stmt {
                check_delete_for_rls(delete, options)?;
            }
            Ok(())
        }
        SetExpr::Values(values) => check_values_for_rls(values, options),
        SetExpr::Table(table) => check_table_command_for_rls(table, options),
        SetExpr::Merge(_) => Ok(()),
    }
}

fn check_limit_clause_for_rls(
    limit_clause: &LimitClause,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    match limit_clause {
        LimitClause::LimitOffset { limit, offset, limit_by } => {
            if let Some(limit) = limit {
                check_expr_for_rls(limit, options)?;
            }
            if let Some(offset) = offset {
                check_expr_for_rls(&offset.value, options)?;
            }
            for expr in limit_by {
                check_expr_for_rls(expr, options)?;
            }
            Ok(())
        }
        LimitClause::OffsetCommaLimit { offset, limit } => {
            check_expr_for_rls(offset, options)?;
            check_expr_for_rls(limit, options)
        }
    }
}

fn check_query_for_rls(query: &Query, options: &Pg2SqliteOptions) -> Result<(), Error> {
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            check_query_for_rls(&cte.query, options)?;
        }
    }
    check_set_expr_for_rls(query.body.as_ref(), options)?;

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        for order_expr in exprs {
            check_expr_for_rls(&order_expr.expr, options)?;
        }
    }

    if let Some(limit_clause) = &query.limit_clause {
        check_limit_clause_for_rls(limit_clause, options)?;
    }

    if let Some(fetch) = &query.fetch
        && let Some(quantity) = &fetch.quantity
    {
        check_expr_for_rls(quantity, options)?;
    }

    Ok(())
}

fn check_function_for_rls(
    function: &sqlparser::ast::Function,
    options: &Pg2SqliteOptions,
) -> Result<(), Error> {
    if let FunctionArguments::List(arg_list) = &function.args {
        for arg in &arg_list.args {
            match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
                | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
                | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => {
                    check_expr_for_rls(expr, options)?;
                }
                _ => {}
            }
        }
    }

    if let Some(filter) = &function.filter {
        check_expr_for_rls(filter, options)?;
    }

    if let Some(over) = &function.over
        && let WindowType::WindowSpec(window_spec) = over
    {
        for expr in &window_spec.partition_by {
            check_expr_for_rls(expr, options)?;
        }
        for order_by_expr in &window_spec.order_by {
            check_expr_for_rls(&order_by_expr.expr, options)?;
        }
    }

    for order_by_expr in &function.within_group {
        check_expr_for_rls(&order_by_expr.expr, options)?;
    }

    Ok(())
}

fn check_insert_for_rls(insert: &Insert, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_table_object_for_rls(&insert.table, options)?;

    if let Some(source) = &insert.source {
        check_query_for_rls(source, options)?;
    }

    Ok(())
}

fn check_update_for_rls(update: &Update, options: &Pg2SqliteOptions) -> Result<(), Error> {
    check_table_factor_for_rls(&update.table.relation, options)?;
    for join in &update.table.joins {
        check_table_factor_for_rls(&join.relation, options)?;
    }

    if let Some(from) = &update.from {
        check_update_from_for_rls(from, options)?;
    }

    if let Some(selection) = &update.selection {
        check_expr_for_rls(selection, options)?;
    }

    for assignment in &update.assignments {
        check_expr_for_rls(&assignment.value, options)?;
    }

    Ok(())
}

fn check_delete_for_rls(delete: &Delete, options: &Pg2SqliteOptions) -> Result<(), Error> {
    for table_name in &delete.tables {
        check_table_for_rls(table_name, options)?;
    }

    check_from_table_for_rls(&delete.from, options)?;

    if let Some(using) = &delete.using {
        check_from_clause_for_rls(using, options)?;
    }

    if let Some(selection) = &delete.selection {
        check_expr_for_rls(selection, options)?;
    }

    Ok(())
}

impl ReverseTranslator for Statement {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type PostgresEntry = Statement;

    fn reverse_translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::PostgresEntry, Error> {
        match self {
            Statement::Insert(insert) => {
                check_insert_for_rls(insert, options)?;

                Ok(Statement::Insert(insert.reverse_translate(schema, options)?))
            }
            Statement::Update(update) => {
                check_update_for_rls(update, options)?;

                Ok(Statement::Update(update.reverse_translate(schema, options)?))
            }
            Statement::Delete(delete) => {
                check_delete_for_rls(delete, options)?;

                Ok(Statement::Delete(delete.reverse_translate(schema, options)?))
            }
            Statement::Query(query) => {
                check_query_for_rls(query, options)?;

                Ok(Statement::Query(Box::new(query.reverse_translate(schema, options)?)))
            }
            // Non-DML statements are not supported for reverse translation
            other => {
                let variant_name = statement_variant_name(other);
                Err(Error::UnsupportedReverseStatement { statement_type: variant_name })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            AccessExpr, Expr, LimitClause, Offset, Query, SetExpr, Statement, Subscript,
            TableFactor,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        check_expr_for_rls, check_limit_clause_for_rls, check_query_for_rls,
        check_set_expr_for_rls, check_table_factor_for_rls,
    };
    use crate::prelude::{Pg2SqliteOptions, ReverseTranslator};

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_expr(expr: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(expr).unwrap().parse_expr().unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmt = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap().remove(0);
        match stmt {
            Statement::Query(query) => *query,
            other => panic!("expected query, got: {other:?}"),
        }
    }

    #[test]
    fn check_expr_for_rls_accepts_many_expression_variants() {
        let options = Pg2SqliteOptions::default();
        let expressions = vec![
            "a = ANY(b)",
            "a = ALL(b)",
            "CASE WHEN x > 0 THEN y ELSE z END",
            "TRIM(BOTH 'x' FROM col)",
            "SUBSTRING(col FROM 1 FOR 2)",
            "OVERLAY(col PLACING 'x' FROM 1 FOR 1)",
            "POSITION('x' IN col)",
            "a AT TIME ZONE 'UTC'",
            "a LIKE b",
            "a ILIKE b",
            "a SIMILAR TO b",
            "a RLIKE b",
            "ARRAY[1,2]",
            "(SELECT 1)",
            "EXISTS (SELECT 1)",
            "x IN (SELECT 1)",
            "(1, 2)",
            "INTERVAL '1 day'",
            "'abc' COLLATE \"C\"",
            "foo[0]",
        ];

        for raw in expressions {
            let expr = parse_expr(raw);
            check_expr_for_rls(&expr, &options).unwrap();
        }
    }

    #[test]
    fn check_expr_for_rls_rejects_subquery_in_subscript_index() {
        let options = Pg2SqliteOptions::default();
        let expr = Expr::CompoundFieldAccess {
            root: Box::new(parse_expr("payload")),
            access_chain: vec![AccessExpr::Subscript(Subscript::Index {
                index: Expr::Subquery(Box::new(parse_query("SELECT id FROM users_rls LIMIT 1"))),
            })],
        };

        let err = check_expr_for_rls(&expr, &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_query_for_rls_covers_with_order_by_limit_fetch_and_function_shapes() {
        let options = Pg2SqliteOptions::default();
        let query = parse_query(
            r#"
            WITH c AS (SELECT 1 AS id)
            SELECT
                id,
                percentile_disc(0.5) WITHIN GROUP (ORDER BY id),
                sum(id) FILTER (WHERE id > 0) OVER (PARTITION BY id ORDER BY id)
            FROM c
            WHERE id IN (SELECT id FROM c)
            GROUP BY id
            HAVING id > 0
            ORDER BY id
            LIMIT 10 OFFSET 1
            FETCH FIRST 5 ROWS ONLY
            "#,
        );

        check_query_for_rls(&query, &options).unwrap();
    }

    #[test]
    fn check_set_expr_for_rls_handles_insert_update_delete_values_and_table_variants() {
        let options = Pg2SqliteOptions::default();

        let insert_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "INSERT INTO users(id) VALUES (1)")
                .unwrap()
                .remove(0);
        if let Statement::Insert(insert) = insert_stmt {
            check_set_expr_for_rls(&SetExpr::Insert(Statement::Insert(insert)), &options).unwrap();
        } else {
            panic!("expected insert");
        }

        let update_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "UPDATE users SET id = 1").unwrap().remove(0);
        if let Statement::Update(update) = update_stmt {
            check_set_expr_for_rls(&SetExpr::Update(Statement::Update(update)), &options).unwrap();
        } else {
            panic!("expected update");
        }

        let delete_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "DELETE FROM users WHERE id = 1")
                .unwrap()
                .remove(0);
        if let Statement::Delete(delete) = delete_stmt {
            check_set_expr_for_rls(&SetExpr::Delete(Statement::Delete(delete)), &options).unwrap();
        } else {
            panic!("expected delete");
        }

        let values_query = parse_query("VALUES (1), (2)");
        check_set_expr_for_rls(values_query.body.as_ref(), &options).unwrap();

        let table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users".to_string()),
            schema_name: None,
        }));
        check_set_expr_for_rls(&table_expr, &options).unwrap();

        let rls_table_expr = SetExpr::Table(Box::new(sqlparser::ast::Table {
            table_name: Some("users_rls".to_string()),
            schema_name: None,
        }));
        let err = check_set_expr_for_rls(&rls_table_expr, &options).unwrap_err();
        assert!(err.to_string().contains("users_rls"));
    }

    #[test]
    fn check_limit_clause_for_rls_handles_offset_comma_limit_variant() {
        let options = Pg2SqliteOptions::default();
        let offset_comma =
            LimitClause::OffsetCommaLimit { offset: parse_expr("1"), limit: parse_expr("10") };
        check_limit_clause_for_rls(&offset_comma, &options).unwrap();

        let limit_offset = LimitClause::LimitOffset {
            limit: Some(parse_expr("10")),
            offset: Some(Offset { value: parse_expr("1"), rows: sqlparser::ast::OffsetRows::None }),
            limit_by: vec![parse_expr("2")],
        };
        check_limit_clause_for_rls(&limit_offset, &options).unwrap();
    }

    #[test]
    fn reverse_translate_rejects_rls_backing_tables_and_non_dml_statements() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();

        let query_stmt =
            Parser::parse_sql(&PostgreSqlDialect {}, "SELECT * FROM users_rls").unwrap().remove(0);
        let err = query_stmt.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Direct access to RLS backing table"));

        let non_dml = Parser::parse_sql(&PostgreSqlDialect {}, "VACUUM").unwrap().remove(0);
        let err = non_dml.reverse_translate(&schema, &options).unwrap_err();
        assert!(err.to_string().contains("Reverse translation only supports DML statements"));
    }

    #[test]
    fn check_table_factor_and_set_expr_cover_query_fallback_paths() {
        let options = Pg2SqliteOptions::default();

        let table_fn_query = parse_query("SELECT * FROM generate_series(1, 2)");
        let sqlparser::ast::SetExpr::Select(select) = table_fn_query.body.as_ref() else {
            panic!("expected select");
        };
        check_table_factor_for_rls(&select.from[0].relation, &options).unwrap();

        let manual_table_function =
            TableFactor::TableFunction { expr: parse_expr("generate_series(1, 2)"), alias: None };
        check_table_factor_for_rls(&manual_table_function, &options).unwrap();

        let set_expr = SetExpr::Query(Box::new(parse_query("SELECT 1")));
        check_set_expr_for_rls(&set_expr, &options).unwrap();
    }
}
