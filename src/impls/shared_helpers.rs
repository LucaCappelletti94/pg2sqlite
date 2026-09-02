//! Shared helper functions for translating table references, joins, and select
//! items. Generic over translation direction (forward or reverse).

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::ops::ControlFlow;

use sql_traits::{
    structs::{ColumnDefinition, ParserDB},
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    Assignment, AssignmentTarget, BinaryOperator, DataType, Expr, ExprWithAlias,
    ExprWithAliasAndOrderBy, Fetch, FromTable, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentClause, FunctionArgumentList, FunctionArguments, GroupByExpr, HavingBound,
    Ident, Join, JoinConstraint, JoinOperator, LimitClause, ListAggOnOverflow, Measure,
    NamedWindowDefinition, NamedWindowExpr, ObjectName, ObjectNamePart, OrderBy, OrderByExpr,
    OrderByKind, PipeOperator, PivotValueSource, Query, SelectItem, SetExpr, SetOperator,
    SetQuantifier, Setting, Statement, SymbolDefinition, TableAlias, TableFactor,
    TableFunctionArgs, TableSample, TableSampleBucket, TableSampleKind, TableSampleQuantity,
    TableVersion, TableWithJoins, UnaryOperator, UpdateTableFromKind, Value, ValueWithSpan, Values,
    Visit, Visitor, WindowFrame, WindowFrameBound, WindowSpec, WindowType, With, WithFill,
    XmlNamespaceDefinition, XmlPassingArgument, XmlPassingClause, XmlTableColumn,
    XmlTableColumnOption, visit_expressions,
};

use crate::{
    errors::Error,
    impls::{
        object_name::{last_ident, resolve_translation_table},
        query_builder::{from_relation, make_query, make_simple_select},
        translator_impls::{
            uuid::{
                is_blob_uuid_representation, maybe_wrap_text_uuid_literal, uuid_columns_of_table,
            },
            vector::{maybe_wrap_text_vector_literal, vector_columns_of_table},
        },
    },
    prelude::Pg2SqliteOptions,
};

/// Abstracts the direction of translation so that shared helper functions
/// can work for both forward (`Translator`) and reverse (`ReverseTranslator`)
/// translation.
pub(crate) trait TranslationDirection {
    /// `true` for forward (PostgreSQL → SQLite) translation, `false` for
    /// reverse.
    const IS_FORWARD: bool = false;
    type Options<'a>;
    fn config<'options>(options: &'options Self::Options<'_>) -> &'options Pg2SqliteOptions;

    fn forward_context<'options, 'config>(
        _options: &'options Self::Options<'config>,
    ) -> Option<&'options crate::options::TranslationContext<'config>> {
        None
    }

    /// The `WITH` clause of the query being translated, so a scope built for
    /// one arm of a set operation keeps a CTE reference opaque.
    fn cte_clause<'options>(
        _options: &'options Self::Options<'_>,
    ) -> Option<&'options sqlparser::ast::With> {
        None
    }

    /// The same options with `scope` attached, which is how a `SELECT` puts its
    /// own relations in scope for the expressions inside it.
    fn with_scope<'scope>(
        options: &'scope Self::Options<'_>,
        scope: &'scope sql_traits::structs::ColumnScope<'scope, 'scope, ParserDB>,
    ) -> Self::Options<'scope>;

    fn translate_expr(
        expr: &Expr,
        schema: &ParserDB,
        options: &Self::Options<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Expr, Error>;
    fn translate_query(
        query: &Query,
        schema: &ParserDB,
        options: &Self::Options<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<Query, Error>;
    fn translate_insert(
        insert: &sqlparser::ast::Insert,
        schema: &ParserDB,
        options: &Self::Options<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Insert, Error>;
    fn translate_delete(
        delete: &sqlparser::ast::Delete,
        schema: &ParserDB,
        options: &Self::Options<'_>,
        emit: crate::warnings::WarningSink<'_>,
    ) -> Result<sqlparser::ast::Delete, Error>;

    fn translate_object_name(
        name: &ObjectName,
        _schema: &ParserDB,
        _options: &Self::Options<'_>,
    ) -> Result<ObjectName, Error> {
        Ok(name.clone())
    }
}
fn required_forward_context<'options, 'config, D: TranslationDirection>(
    options: &'options D::Options<'config>,
) -> &'options crate::options::TranslationContext<'config> {
    D::forward_context(options).expect("forward translation context")
}

/// Shared unsupported-feature message for `generate_series` usage.
pub(crate) const GENERATE_SERIES_UNSUPPORTED_MESSAGE: &str = "generate_series() is not available in standard SQLite. \
     Use a recursive CTE instead: \
     WITH RECURSIVE s(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM s WHERE n < N) SELECT n FROM s";

/// Returns `true` when an object name resolves to `generate_series`.
#[must_use]
pub(crate) fn is_generate_series_object_name(name: &ObjectName) -> bool {
    name.0
        .last()
        .and_then(|part| {
            if let ObjectNamePart::Identifier(id) = part { Some(id.value.as_str()) } else { None }
        })
        .is_some_and(|value| value.eq_ignore_ascii_case("generate_series"))
}

/// Returns the standardized error for unsupported `generate_series`.
#[must_use]
pub(crate) fn generate_series_not_supported_error() -> Error {
    Error::forward_refusal(GENERATE_SERIES_UNSUPPORTED_MESSAGE.to_string())
}

/// Returns the standardised error for `WITH ORDINALITY`, which SQLite has no
/// clause for.
///
/// Refused rather than dropped, because the ordinality column is projected by
/// the query around it, so losing the clause loses a column the caller selects.
/// `UNNEST ... WITH ORDINALITY` does NOT come here: forward translation lowers
/// it onto `json_each`, whose `key` column supplies the ordinality.
#[must_use]
pub(crate) fn with_ordinality_not_supported_error() -> Error {
    Error::forward_refusal(
        "WITH ORDINALITY is not supported in SQLite, which has no clause that numbers the rows of \
     a FROM item. Number them in the query instead, with ROW_NUMBER() OVER (), or use UNNEST, \
     which is translated through json_each and does supply an ordinality column."
            .to_string(),
    )
}

/// Returns the standardised error for `NULLS NOT DISTINCT`.
///
/// PostgreSQL makes two NULL rows collide under it, and SQLite's unique
/// indexes always treat NULLs as distinct, with no clause to change that.
/// Verified on both: PostgreSQL 16 answers `duplicate key value violates
/// unique constraint` for a second NULL, SQLite accepts it. So the clause
/// cannot be dropped, which would let through rows PostgreSQL refuses, and it
/// cannot be emitted either, which is `near "NULLS": syntax error`.
///
/// `NULLS DISTINCT`, PostgreSQL's default, IS what SQLite does, so that
/// spelling is dropped rather than refused.
#[must_use]
pub(crate) fn nulls_not_distinct_not_supported_error() -> Error {
    Error::forward_refusal("NULLS NOT DISTINCT is not supported in SQLite, whose unique indexes always treat NULLs as \
     distinct, so the constraint would accept rows PostgreSQL rejects. Add a CHECK that the \
     column is NOT NULL, or enforce the rule with a trigger."
        .to_string())
}

/// Returns the standardised error for `MATCH PARTIAL` on a foreign key.
///
/// PostgreSQL 17 refuses the clause itself, with `MATCH PARTIAL not yet
/// implemented`, so no valid PostgreSQL input carries one. SQLite parses a
/// MATCH clause and then always behaves as `MATCH SIMPLE`, so emitting this
/// one would claim an enforcement neither engine implements.
#[must_use]
pub(crate) fn match_partial_not_supported_error() -> Error {
    Error::forward_refusal(
        "FOREIGN KEY ... MATCH PARTIAL cannot be translated. PostgreSQL does not implement it \
     either, answering `MATCH PARTIAL not yet implemented`, and SQLite ignores every MATCH \
     clause, so the emitted constraint would enforce nothing. Use MATCH FULL, which is \
     translated, or the default MATCH SIMPLE."
            .to_string(),
    )
}

/// The name of the column `expr` refers to.
///
/// The qualifier of a compound name is dropped, since it may be an alias rather
/// than a table.
pub(crate) fn referenced_column_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.as_str()),
        Expr::CompoundIdentifier(parts) => Some(parts.last()?.value.as_str()),
        Expr::Nested(inner) => referenced_column_name(inner),
        _ => None,
    }
}

/// The complete column references in an expression, or an explicit unknown
/// result when resolving names requires query scope.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ColumnReferences {
    Complete(Vec<String>),
    Unknown,
}

impl ColumnReferences {
    fn extend(&mut self, other: Self) {
        match other {
            Self::Unknown => *self = Self::Unknown,
            Self::Complete(mut additional) => {
                if let Self::Complete(columns) = self {
                    columns.append(&mut additional);
                }
            }
        }
    }
}

struct FunctionColumnCollector {
    columns: Vec<String>,
}

impl Visitor for FunctionColumnCollector {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        ControlFlow::Break(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        match expr {
            Expr::Identifier(ident) => self.columns.push(ident.value.clone()),
            Expr::CompoundIdentifier(idents) => {
                if let Some(ident) = idents.last() {
                    self.columns.push(ident.value.clone());
                }
            }
            Expr::Wildcard(_) | Expr::QualifiedWildcard(..) | Expr::MatchAgainst { .. } => {
                return ControlFlow::Break(());
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// Returns every column name `expr` mentions.
#[must_use]
pub(crate) fn extract_columns_from_expr(expr: &Expr) -> ColumnReferences {
    match expr {
        Expr::Identifier(ident) => ColumnReferences::Complete(vec![ident.value.clone()]),
        Expr::CompoundIdentifier(idents) => {
            ColumnReferences::Complete(
                idents.last().map(|ident| vec![ident.value.clone()]).unwrap_or_default(),
            )
        }
        Expr::Function(function) => extract_columns_from_function(function),
        Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..)
        | Expr::MatchAgainst { .. } => ColumnReferences::Unknown,
        _ => {
            let mut columns = ColumnReferences::Complete(Vec::new());
            crate::impls::expr_helpers::for_each_child_expr(expr, &mut |child| {
                columns.extend(extract_columns_from_expr(child));
            });
            columns
        }
    }
}

/// Returns every column name in a function, unless it contains a query.
#[must_use]
pub(crate) fn extract_columns_from_function(function: &Function) -> ColumnReferences {
    let mut collector = FunctionColumnCollector { columns: Vec::new() };
    match function.visit(&mut collector) {
        ControlFlow::Continue(()) => ColumnReferences::Complete(collector.columns),
        ControlFlow::Break(()) => ColumnReferences::Unknown,
    }
}

/// The declared type of the column `expr` names, read through the relations in
/// scope.
///
/// Three answers, and the difference between the last two is what keeps a guess
/// out of the output:
///
/// - `Ok(None)` when `expr` is not a column reference, so there is nothing to
///   resolve and nothing to refuse,
/// - `Ok(Some(_))` when the scope resolves the reference and `read` accepts the
///   declared type, or `Ok(None)` when `read` declines it,
/// - an error when `expr` is a reference the relations in scope cannot answer.
///   That case used to be answered by scanning every table in the schema for a
///   column of the same name, which reads another table's type when the names
///   collide.
///
/// `read` needs the structured type, so the parsed DDL is read directly rather
/// than through `ColumnLike::data_type`, which answers a normalised token.
pub(crate) fn declared_in_scope<T: PartialEq>(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    read_type: impl Fn(&DataType) -> Option<T>,
    read_expression: impl Fn(
        &Expr,
        &ParserDB,
        &crate::options::TranslationContext<'_>,
    ) -> Result<Option<T>, crate::errors::Error>,
) -> Result<Option<T>, crate::errors::Error> {
    let reference = strip_nesting(expr);
    let bare_column;
    let pseudo_row_reference = match reference {
        Expr::CompoundIdentifier(parts)
            if parts.len() == 2
                && matches!(parts[0].value.to_ascii_uppercase().as_str(), "NEW" | "OLD") =>
        {
            bare_column = Expr::Identifier(parts[1].clone());
            Some(&bare_column)
        }
        _ => None,
    };
    let Some(column_name) = referenced_column_name(pseudo_row_reference.unwrap_or(reference))
    else {
        return Ok(None);
    };
    if column_name.eq_ignore_ascii_case("rowid")
        || column_name == crate::impls::translator_impls::plpgsql::VARIABLE_VALUE_COLUMN
        || options.is_variable(column_name)
    {
        return Ok(None);
    }

    let mut tried_any = false;
    for definition in options.column_definitions(reference, pseudo_row_reference) {
        tried_any = true;
        let Some(definition) = definition.map_err(|error| {
            unresolved_reference(
                reference,
                &format!("more than one relation in scope exposes it ({error})"),
            )
        })?
        else {
            continue;
        };
        return match evaluate_definition(
            &definition,
            schema,
            options,
            &read_type,
            &read_expression,
        )? {
            DefinitionValue::Known(value) => Ok(value),
            DefinitionValue::Opaque => {
                Err(unresolved_reference(
                    reference,
                    "the relation exposes the column without an inspectable definition",
                ))
            }
        };
    }

    Err(unresolved_reference(
        reference,
        if tried_any {
            "no relation in scope declares it"
        } else {
            "no relation is in scope where this expression appears"
        },
    ))
}

enum DefinitionValue<T> {
    Known(Option<T>),
    Opaque,
}

fn evaluate_definition<T: PartialEq>(
    definition: &ColumnDefinition<'_, '_, '_, ParserDB>,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    read_type: &impl Fn(&DataType) -> Option<T>,
    read_expression: &impl Fn(
        &Expr,
        &ParserDB,
        &crate::options::TranslationContext<'_>,
    ) -> Result<Option<T>, crate::errors::Error>,
) -> Result<DefinitionValue<T>, crate::errors::Error> {
    match definition {
        ColumnDefinition::Base { column, .. } => {
            Ok(DefinitionValue::Known(read_type(&column.attribute().data_type)))
        }
        ColumnDefinition::Expression { expression, scope } => {
            let scoped = options.with_definition_scope(*scope);
            Ok(DefinitionValue::Known(read_expression(expression, schema, &scoped)?))
        }
        ColumnDefinition::SetOperation { left, right, .. } => {
            let left = evaluate_definition(
                &left.definition(),
                schema,
                options,
                read_type,
                read_expression,
            )?;
            let right = evaluate_definition(
                &right.definition(),
                schema,
                options,
                read_type,
                read_expression,
            )?;
            Ok(match (left, right) {
                (DefinitionValue::Known(left), DefinitionValue::Known(right)) if left == right => {
                    DefinitionValue::Known(left)
                }
                (DefinitionValue::Opaque, _) | (_, DefinitionValue::Opaque) => {
                    DefinitionValue::Opaque
                }
                _ => DefinitionValue::Known(None),
            })
        }
        ColumnDefinition::RecursiveUnion { anchor, .. } => {
            evaluate_definition(&anchor.definition(), schema, options, read_type, read_expression)
        }
        ColumnDefinition::Opaque => Ok(DefinitionValue::Opaque),
    }
}

/// The query a column scope should be built from, when the written one has a
/// shape the resolver reads as opaque.
///
/// A nested join is one: `FROM (a JOIN b) JOIN c` hides `a` and `b`, so the
/// relations are flattened into a list. The `WITH` clause travels along, since
/// a reference to a CTE must stay unresolvable rather than match a base table
/// of the same name. `None` means the written query needs no substitute.
pub(crate) fn scope_query_for(query: &Query) -> Option<Query> {
    let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    if !select.from.iter().any(has_nested_join) {
        return None;
    }
    let mut flattened = Vec::new();
    for entry in &select.from {
        flatten_relations(entry, &mut flattened);
    }
    let mut substitute = relations_scope_query(flattened);
    substitute.with.clone_from(&query.with);
    Some(substitute)
}
fn scope_query_after_factor_rewrites(
    select: &sqlparser::ast::Select,
    translated_from: &[sqlparser::ast::TableWithJoins],
    with: Option<sqlparser::ast::With>,
) -> Option<Query> {
    fn replace_factor(
        written: &mut sqlparser::ast::TableFactor,
        translated: &sqlparser::ast::TableFactor,
    ) -> bool {
        match (&mut *written, translated) {
            (
                sqlparser::ast::TableFactor::Table { args: Some(_), .. }
                | sqlparser::ast::TableFactor::Function { .. }
                | sqlparser::ast::TableFactor::UNNEST { .. },
                sqlparser::ast::TableFactor::Derived { .. },
            ) => {
                *written = translated.clone();
                true
            }
            (
                sqlparser::ast::TableFactor::NestedJoin { table_with_joins: written, .. },
                sqlparser::ast::TableFactor::NestedJoin { table_with_joins: translated, .. },
            ) => replace_entry(written, translated),
            _ => false,
        }
    }

    fn replace_entry(
        written: &mut sqlparser::ast::TableWithJoins,
        translated: &sqlparser::ast::TableWithJoins,
    ) -> bool {
        let mut changed = replace_factor(&mut written.relation, &translated.relation);
        for (written, translated) in written.joins.iter_mut().zip(&translated.joins) {
            changed |= replace_factor(&mut written.relation, &translated.relation);
        }
        changed
    }

    let mut relations = select.from.clone();
    let mut changed = false;
    for (written, translated) in relations.iter_mut().zip(translated_from) {
        changed |= replace_entry(written, translated);
    }
    if !changed {
        return None;
    }

    let mut scoped_select = select.clone();
    scoped_select.from = relations;
    let query = crate::impls::query_builder::make_query(
        with,
        sqlparser::ast::SetExpr::Select(Box::new(scoped_select)),
    );
    scope_query_for(&query).or(Some(query))
}

fn has_nested_join(entry: &sqlparser::ast::TableWithJoins) -> bool {
    core::iter::once(&entry.relation)
        .chain(entry.joins.iter().map(|join| &join.relation))
        .any(|factor| matches!(factor, sqlparser::ast::TableFactor::NestedJoin { .. }))
}

/// Appends `entry` and everything a nested join inside it hides.
fn flatten_relations(
    entry: &sqlparser::ast::TableWithJoins,
    out: &mut Vec<sqlparser::ast::TableWithJoins>,
) {
    let mut factors = vec![&entry.relation];
    factors.extend(entry.joins.iter().map(|join| &join.relation));
    for factor in factors {
        match factor {
            sqlparser::ast::TableFactor::NestedJoin { table_with_joins, .. } => {
                flatten_relations(table_with_joins, out);
            }
            other => {
                out.push(sqlparser::ast::TableWithJoins { relation: other.clone(), joins: vec![] });
            }
        }
    }
}

/// A query whose `FROM` carries `relations`, so a statement that has relations
/// but no query of its own can build a column scope with the same resolver a
/// `SELECT` uses.
///
/// `DELETE ... USING` and `UPDATE ... FROM` are the cases: an unqualified
/// reference names the target, and a qualified one may name any relation the
/// statement lists.
pub(crate) fn relations_scope_query(
    relations: Vec<sqlparser::ast::TableWithJoins>,
) -> sqlparser::ast::Query {
    crate::impls::query_builder::make_query(
        None,
        sqlparser::ast::SetExpr::Select(alloc::boxed::Box::new(
            crate::impls::query_builder::make_simple_select(
                vec![sqlparser::ast::SelectItem::Wildcard(
                    sqlparser::ast::WildcardAdditionalOptions::default(),
                )],
                relations,
                None,
            ),
        )),
    )
}

/// Unwraps parentheses, which carry no meaning for resolution.
fn strip_nesting(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner) => strip_nesting(inner),
        other => other,
    }
}

fn unresolved_reference(reference: &Expr, reason: &str) -> crate::errors::Error {
    crate::errors::Error::UnresolvedColumnReference {
        reference: reference.to_string(),
        reason: reason.to_string(),
    }
}

/// True when the column `expr` names is declared with a type `predicate`
/// accepts.
pub(crate) fn declared_type_matches(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    predicate: impl Fn(&str) -> bool + Copy,
) -> Result<bool, crate::errors::Error> {
    Ok(declared_in_scope(
        expr,
        schema,
        options,
        |data_type| predicate(&data_type.to_string()).then_some(()),
        |expression, schema, options| {
            Ok(declared_type_matches(expression, schema, options, predicate)?.then_some(()))
        },
    )?
    .is_some())
}

/// True when `expr` is a whole number by construction, so its scale is 0.
pub(crate) fn is_integral_expression(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<bool, crate::errors::Error> {
    match expr {
        Expr::Nested(inner) => is_integral_expression(inner, schema, options),
        Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr } => {
            is_integral_expression(expr, schema, options)
        }
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => {
            Ok(!digits.contains('.') && !digits.contains(['e', 'E']))
        }
        Expr::Cast { data_type, .. } => Ok(matches!(data_type, DataType::Integer(_))),
        _ => {
            declared_type_matches(expr, schema, options, |declared| {
                let lowered = declared.to_ascii_lowercase();
                ["int", "smallint", "bigint", "serial"]
                    .iter()
                    .any(|integral| lowered.starts_with(integral))
            })
        }
    }
}

/// The scale of `expr` when it is a `NUMERIC` value held as minor units.
pub(crate) fn numeric_scale(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<u32>, crate::errors::Error> {
    Ok(numeric_precision_and_scale_of(expr, schema, options)?.map(|(_, scale)| scale))
}

/// The declared precision of `expr`, which D1's multiplication rule needs.
pub(crate) fn declared_numeric_precision(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<u64>, crate::errors::Error> {
    Ok(numeric_precision_and_scale_of(expr, schema, options)?.map(|(precision, _)| precision))
}

fn numeric_precision_and_scale_of(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<(u64, u32)>, crate::errors::Error> {
    let read = |data_type: &DataType| {
        let info = crate::impls::translator_impls::data_type::exact_numeric_info(data_type)?;
        crate::impls::translator_impls::data_type::numeric_precision_and_scale(info).ok()
    };
    match expr {
        Expr::Nested(inner)
        | Expr::UnaryOp { op: UnaryOperator::Minus | UnaryOperator::Plus, expr: inner } => {
            numeric_precision_and_scale_of(inner, schema, options)
        }
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. })
            if !digits.contains('.') && !digits.contains(['e', 'E']) =>
        {
            Ok(u64::try_from(digits.len()).ok().map(|precision| (precision, 0)))
        }
        Expr::Cast { data_type, .. } => Ok(read(data_type)),
        _ => declared_in_scope(expr, schema, options, read, numeric_precision_and_scale_of),
    }
}

/// Move a value held as minor units from `from` scale to `to` scale.
///
/// Growing the scale multiplies. Shrinking it divides, and PostgreSQL rounds
/// half away from zero where SQLite's integer division truncates toward it, so
/// half a unit is added with the value's sign first. `1.005::numeric(10,2)` is
/// 1.01 and `(-1.005)::numeric(10,2)` is -1.01.
pub(crate) fn rescale_minor_units(value: Expr, from: u32, to: u32) -> Expr {
    if from == to {
        return value;
    }
    if to > from {
        return Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(value),
            op: BinaryOperator::Multiply,
            right: Box::new(crate::impls::function_helpers::number_literal(
                &10_i128.pow(to - from).to_string(),
            )),
        }));
    }

    let divisor = 10_i128.pow(from - to);
    let half = divisor / 2;
    // `value + half * sign(value)` before the truncating division turns
    // truncation toward zero into rounding away from it.
    let biased = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(value.clone()),
        op: BinaryOperator::Plus,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(crate::impls::function_helpers::number_literal(&half.to_string())),
            op: BinaryOperator::Multiply,
            right: Box::new(crate::impls::function_helpers::simple_function_expr(
                "sign",
                vec![value],
                None,
            )),
        }),
    }));
    Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(biased),
        op: BinaryOperator::Divide,
        right: Box::new(crate::impls::function_helpers::number_literal(&divisor.to_string())),
    }))
}

/// Rewrite a decimal literal as the integer count of minor units at `scale`.
///
/// The digits are moved rather than multiplied as a float, so `19.99` at scale
/// 2 is 1999 and not 1998.9999999999998. A literal finer than the column is
/// refused rather than rounded.
pub(crate) fn scale_decimal_literal(expr: &Expr, scale: u32) -> Result<Option<Expr>, Error> {
    let (negated, digits) = match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => (false, digits),
        Expr::UnaryOp { op: UnaryOperator::Minus, expr } => {
            match expr.as_ref() {
                Expr::Value(ValueWithSpan { value: Value::Number(digits, _), .. }) => {
                    (true, digits)
                }
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };

    if digits.contains(['e', 'E']) {
        return Err(Error::forward_refusal(format!(
            "the literal {digits} is in exponent notation, which this translator does not scale \
             onto a NUMERIC column. Write it in full."
        )));
    }

    let (whole, fraction) = digits.split_once('.').unwrap_or((digits.as_str(), ""));
    let fraction_digits = u32::try_from(fraction.len()).unwrap_or(u32::MAX);
    if fraction_digits > scale {
        return Err(Error::forward_refusal(format!(
            "the literal {digits} has {fraction_digits} decimal places and the column holds \
             {scale}. PostgreSQL would round it, which silently changes the value, so write it \
             at the column's scale instead."
        )));
    }

    // Provably in range: `fraction_digits <= scale` was just checked, and a
    // scale is at most MAX_NUMERIC_PRECISION.
    let padding = "0".repeat(usize::try_from(scale - fraction_digits).unwrap_or(0));
    let minor_units = format!("{}{whole}{fraction}{padding}", if negated { "-" } else { "" });
    Ok(Some(Expr::Value(ValueWithSpan {
        value: Value::Number(minor_units, false),
        span: sqlparser::tokenizer::Span::empty(),
    })))
}

/// The minor-unit scale of a declared type, or `None` when it is not a
/// `NUMERIC` that carries one.
pub(crate) fn minor_unit_scale(data_type: &DataType) -> Option<u32> {
    let info = crate::impls::translator_impls::data_type::exact_numeric_info(data_type)?;
    let (_, scale) =
        crate::impls::translator_impls::data_type::numeric_precision_and_scale(info).ok()?;
    (scale > 0).then_some(scale)
}

/// The minor-unit scale of every scaled column `table` declares.
///
/// Table-based like its siblings `vector_columns_of_table` and
/// `uuid_columns_of_table`, so a caller that already resolved the table does
/// not resolve it again.
pub(crate) fn numeric_minor_unit_scales_of_table(
    table: &<ParserDB as DatabaseLike>::Table,
    schema: &ParserDB,
) -> Vec<(String, u32)> {
    let Ok(columns) = table.columns(schema) else { return Vec::new() };
    columns
        .filter_map(|column| {
            minor_unit_scale(&column.attribute().data_type)
                .map(|scale| (column.column_name().to_string(), scale))
        })
        .collect()
}

/// Every column-typed rewrite a value must take before it is written into a
/// table: a vector text literal becomes a `vec_f32` or `vec_f16` call, a uuid
/// text literal becomes a 16-byte blob conversion under the blob
/// representation, and a literal for a scaled `NUMERIC` column moves onto its
/// minor-unit scale.
///
/// One home rather than one copy per writer. The per-assignment loop existed
/// in three places and drifted twice: R110 found all three missing the
/// scaling, R115 found two still missing the wraps.
#[derive(Default)]
pub(crate) struct ColumnRewrites {
    /// Vector columns, each with whether it is a halfvec.
    vector_cols: Vec<(String, bool)>,
    /// UUID columns, collected only under the blob representation, so an
    /// empty list already encodes the option.
    uuid_cols: Vec<String>,
    /// Scaled `NUMERIC` columns and their minor-unit scales.
    pub(crate) numeric_scales: Vec<(String, u32)>,
}

impl ColumnRewrites {
    /// The rewrites `table`'s declared columns require.
    pub(crate) fn of_table(
        table: &<ParserDB as DatabaseLike>::Table,
        schema: &ParserDB,
        options: &Pg2SqliteOptions,
    ) -> Self {
        Self {
            vector_cols: vector_columns_of_table(table, schema).unwrap_or_default(),
            uuid_cols: if is_blob_uuid_representation(options) {
                uuid_columns_of_table(table, schema).unwrap_or_default()
            } else {
                Vec::new()
            },
            numeric_scales: numeric_minor_unit_scales_of_table(table, schema),
        }
    }

    /// The rewrites for the named table, or none when it does not resolve,
    /// which leaves the caller emitting what it was handed rather than
    /// rewriting against a guess.
    pub(crate) fn for_named_table(
        schema: &ParserDB,
        table_name: &ObjectName,
        options: &Pg2SqliteOptions,
    ) -> Self {
        match resolve_translation_table(schema, table_name) {
            Ok(Some(table)) => Self::of_table(table, schema, options),
            _ => Self::default(),
        }
    }

    fn is_empty(&self) -> bool {
        self.vector_cols.is_empty() && self.uuid_cols.is_empty() && self.numeric_scales.is_empty()
    }

    /// Finishes a translated value written into `column`.
    ///
    /// Each rewrite touches only a literal and leaves every other shape
    /// alone, so `excluded.col`, an already-wrapped call, and an expression
    /// the translator already scaled all pass through unchanged.
    pub(crate) fn finish_value(
        &self,
        column: &str,
        value: Expr,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error> {
        let mut value = if let Some(is_halfvec) = self
            .vector_cols
            .iter()
            .find(|(col, _)| col.eq_ignore_ascii_case(column))
            .map(|(_, is_halfvec)| *is_halfvec)
        {
            maybe_wrap_text_vector_literal(value, is_halfvec)
        } else if self.uuid_cols.iter().any(|col| col.eq_ignore_ascii_case(column)) {
            maybe_wrap_text_uuid_literal(value, options)?
        } else {
            value
        };
        scale_literal_for_column(&mut value, column, &self.numeric_scales)?;
        Ok(value)
    }

    /// Finishes every value an assignment writes.
    ///
    /// The tuple spelling, `SET (a, b) = (1, 2)`, is zipped name by name.
    /// SQLite accepts that shape, so skipping it would store the wrong value
    /// rather than fail.
    pub(crate) fn finish_assignment(
        &self,
        target: &AssignmentTarget,
        value: Expr,
        options: &Pg2SqliteOptions,
    ) -> Result<Expr, Error> {
        if self.is_empty() {
            return Ok(value);
        }
        match target {
            AssignmentTarget::ColumnName(name) => {
                match last_ident(name) {
                    Some(column) => self.finish_value(&column.value, value, options),
                    None => Ok(value),
                }
            }
            AssignmentTarget::Tuple(names) => {
                let Expr::Tuple(items) = value else { return Ok(value) };
                if items.len() != names.len() {
                    return Ok(Expr::Tuple(items));
                }
                names
                    .iter()
                    .zip(items)
                    .map(|(name, item)| {
                        match last_ident(name) {
                            Some(column) => self.finish_value(&column.value, item, options),
                            None => Ok(item),
                        }
                    })
                    .collect::<Result<Vec<_>, Error>>()
                    .map(Expr::Tuple)
            }
        }
    }
}

/// Rewrites a literal written into `column` as minor units, in place.
///
/// Anything that is not a literal is left alone, which is what stops an
/// expression the translator already scaled from being scaled a second time.
pub(crate) fn scale_literal_for_column(
    value: &mut Expr,
    column: &str,
    scales: &[(String, u32)],
) -> Result<(), Error> {
    let Some((_, scale)) = scales.iter().find(|(name, _)| name.eq_ignore_ascii_case(column)) else {
        return Ok(());
    };
    if let Some(scaled) = scale_decimal_literal(value, *scale)? {
        *value = scaled;
    }
    Ok(())
}

/// Translates DO UPDATE assignments and WHERE inside an ON CONFLICT clause.
///
/// `rewrites` carries the target table's column-typed rewrites, since a DO
/// UPDATE writes into the same columns the insert does. Empty for the reverse
/// direction, which never unwraps or unscales.
pub(crate) fn translate_on_conflict_do_update<D: TranslationDirection>(
    on_conflict: &sqlparser::ast::OnConflict,
    do_update: &sqlparser::ast::DoUpdate,
    schema: &ParserDB,
    options: &D::Options<'_>,
    rewrites: &ColumnRewrites,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::OnInsert, Error> {
    let assignments = do_update
        .assignments
        .iter()
        .map(|a| {
            let value = D::translate_expr(&a.value, schema, options, emit)?;
            Ok(Assignment {
                target: a.target.clone(),
                value: rewrites.finish_assignment(&a.target, value, D::config(options))?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let selection = do_update
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;
    Ok(sqlparser::ast::OnInsert::OnConflict(sqlparser::ast::OnConflict {
        conflict_target: on_conflict.conflict_target.clone(),
        action: sqlparser::ast::OnConflictAction::DoUpdate(sqlparser::ast::DoUpdate {
            assignments,
            selection,
        }),
    }))
}

/// True when `expr` is the bare `DEFAULT` keyword.
///
/// `sqlparser` has no `Expr` variant for it: `DEFAULT` is not reserved in
/// expression position, so it arrives as a plain identifier, which is why this
/// is a name comparison rather than a pattern match. A column genuinely called
/// `default` has to be quoted to be referenced, and a quoted identifier is not
/// matched here.
#[must_use]
pub(crate) fn is_default_keyword(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Identifier(ident)
            if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("default")
    )
}

/// True when `expr` carries the `DEFAULT` keyword, directly or as a tuple
/// element.
#[must_use]
pub(crate) fn carries_default_keyword(expr: &Expr) -> bool {
    match expr {
        Expr::Tuple(items) => items.iter().any(is_default_keyword),
        other => is_default_keyword(other),
    }
}

/// Substitutes the declared default for a `DEFAULT` keyword an assignment
/// writes, or answers `None` when there is nothing to substitute.
///
/// PostgreSQL accepts the keyword in an UPDATE assignment and in a DO UPDATE
/// list, storing the declared default, NULL when none is declared, measured
/// on PostgreSQL 16. The substituted expression is the raw PostgreSQL default,
/// so the caller's ordinary translate and finish pipeline scales and wraps it
/// like any written value. The tuple spelling substitutes per position.
pub(crate) fn substituted_assignment_default(
    target: &AssignmentTarget,
    value: &Expr,
    table: &<ParserDB as DatabaseLike>::Table,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Expr>, Error> {
    let mut default_for = |name: &ObjectName| -> Result<Expr, Error> {
        match last_ident(name) {
            Some(column) => {
                crate::impls::translator_impls::insert::default_expr_for_column(
                    table,
                    &column.value,
                    schema,
                    options,
                    emit,
                )
            }
            None => Err(default_outside_an_insert_error()),
        }
    };

    match target {
        AssignmentTarget::ColumnName(name) => {
            if is_default_keyword(value) {
                default_for(name).map(Some)
            } else {
                Ok(None)
            }
        }
        AssignmentTarget::Tuple(names) => {
            let Expr::Tuple(items) = value else { return Ok(None) };
            if items.len() != names.len() || !items.iter().any(is_default_keyword) {
                return Ok(None);
            }
            names
                .iter()
                .zip(items)
                .map(
                    |(name, item)| {
                        if is_default_keyword(item) { default_for(name) } else { Ok(item.clone()) }
                    },
                )
                .collect::<Result<Vec<_>, Error>>()
                .map(|items| Some(Expr::Tuple(items)))
        }
    }
}

/// Returns the error for a `DEFAULT` with no column to read a default from.
///
/// A VALUES row of an INSERT and an UPDATE assignment both tie the keyword to
/// a column and substitute the declared default before translation. Anything
/// reaching here is in a position with no column, which PostgreSQL rejects
/// too, or names a table the schema does not hold.
#[must_use]
pub(crate) fn default_outside_an_insert_error() -> Error {
    Error::forward_refusal(
        "DEFAULT stands for a column's declared default, and only a VALUES row of an INSERT or \
     an UPDATE assignment on a declared table ties it to a column. PostgreSQL rejects it in \
     other positions too, and SQLite has no form of it at all."
            .to_string(),
    )
}

/// Extracts a stable-ish variant name from debug output.
#[must_use]
pub(crate) fn debug_variant_name(value: &impl core::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    debug.split(['(', '{', ' ']).next().unwrap_or("Unknown").to_string()
}

/// Translates `expr` using direction `D`, delegating structural recursion to
/// [`crate::impls::expr_helpers::try_map_expr_children`]. Callers should handle
/// direction-specific semantic transforms before falling through.
pub(crate) fn translate_expr_recursive<D: TranslationDirection>(
    expr: &Expr,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Expr, Error> {
    let emit = core::cell::RefCell::new(emit);
    crate::impls::expr_helpers::try_map_expr_children(
        expr,
        &mut |e| D::translate_expr(e, schema, options, &mut **emit.borrow_mut()),
        &mut |q| D::translate_query(q, schema, options, &mut **emit.borrow_mut()),
    )
}

/// Translate the core fields shared by forward and reverse `Delete`
/// translation: `selection`, `from`, `returning`, `order_by`, and `limit`.
#[allow(clippy::type_complexity)]
pub(crate) fn translate_delete_core<D: TranslationDirection>(
    delete: &sqlparser::ast::Delete,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<(Option<Expr>, FromTable, Option<Vec<SelectItem>>, Vec<OrderByExpr>, Option<Expr>), Error>
{
    let selection = delete
        .selection
        .as_ref()
        .map(|e| D::translate_expr(e, schema, options, emit))
        .transpose()?;
    let from = map_from_table(&delete.from, |table| {
        translate_table_with_joins::<D>(table, schema, options, emit)
    })?;
    let returning = translate_returning::<D>(delete.returning.as_ref(), schema, options, emit)?;
    let order_by = delete
        .order_by
        .iter()
        .map(|expr| translate_order_by_expr::<D>(expr, schema, options, emit))
        .collect::<Result<Vec<_>, _>>()?;
    let limit = delete
        .limit
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;
    Ok((selection, from, returning, order_by, limit))
}

/// Returns the expression for argument variants that carry one.
#[must_use]
pub(crate) fn function_arg_expr(arg: &FunctionArg) -> Option<&Expr> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
        | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. }
        | FunctionArg::ExprNamed { arg: FunctionArgExpr::Expr(expr), .. } => Some(expr),
        _ => None,
    }
}

/// Collects expression payloads from function arguments.
#[must_use]
pub(crate) fn function_argument_exprs(args: &FunctionArguments) -> Vec<&Expr> {
    match args {
        FunctionArguments::List(list) => list.args.iter().filter_map(function_arg_expr).collect(),
        _ => Vec::new(),
    }
}

/// Translates all function arguments, recursively translating any expression or
/// subquery payloads.
pub(crate) fn translate_function_arguments<D: TranslationDirection>(
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<FunctionArguments, Error> {
    match args {
        FunctionArguments::None => Ok(FunctionArguments::None),
        FunctionArguments::Subquery(query) => {
            Ok(FunctionArguments::Subquery(Box::new(D::translate_query(
                query, schema, options, emit,
            )?)))
        }
        FunctionArguments::List(list) => {
            let translated = list
                .args
                .iter()
                .map(|arg| translate_function_arg::<D>(arg, schema, options, emit))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: list.duplicate_treatment,
                args: translated,
                clauses: translate_function_argument_clauses::<D>(
                    &list.clauses,
                    schema,
                    options,
                    emit,
                )?,
            }))
        }
    }
}

fn translate_function_arg_expr<D: TranslationDirection>(
    arg: &FunctionArgExpr,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<FunctionArgExpr, Error> {
    Ok(match arg {
        FunctionArgExpr::Expr(expr) => {
            FunctionArgExpr::Expr(D::translate_expr(expr, schema, options, emit)?)
        }
        FunctionArgExpr::QualifiedWildcard(name) => {
            FunctionArgExpr::QualifiedWildcard(name.clone())
        }
        FunctionArgExpr::Wildcard => FunctionArgExpr::Wildcard,
        FunctionArgExpr::WildcardWithOptions(opts) => {
            FunctionArgExpr::WildcardWithOptions(opts.clone())
        }
    })
}

fn translate_function_arg<D: TranslationDirection>(
    arg: &FunctionArg,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<FunctionArg, Error> {
    Ok(match arg {
        FunctionArg::Named { name, arg, operator } => {
            FunctionArg::Named {
                name: name.clone(),
                arg: translate_function_arg_expr::<D>(arg, schema, options, emit)?,
                operator: operator.clone(),
            }
        }
        FunctionArg::ExprNamed { name, arg, operator } => {
            FunctionArg::ExprNamed {
                name: D::translate_expr(name, schema, options, emit)?,
                arg: translate_function_arg_expr::<D>(arg, schema, options, emit)?,
                operator: operator.clone(),
            }
        }
        FunctionArg::Unnamed(arg) => {
            FunctionArg::Unnamed(translate_function_arg_expr::<D>(arg, schema, options, emit)?)
        }
    })
}

pub(crate) fn translate_setting<D: TranslationDirection>(
    setting: &Setting,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Setting, Error> {
    Ok(Setting {
        key: setting.key.clone(),
        value: D::translate_expr(&setting.value, schema, options, emit)?,
    })
}

fn translate_table_function_args<D: TranslationDirection>(
    args: &TableFunctionArgs,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableFunctionArgs, Error> {
    Ok(TableFunctionArgs {
        args: args
            .args
            .iter()
            .map(|arg| translate_function_arg::<D>(arg, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?,
        settings: args
            .settings
            .as_ref()
            .map(|settings| {
                settings
                    .iter()
                    .map(|setting| translate_setting::<D>(setting, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
    })
}

fn translate_table_version<D: TranslationDirection>(
    version: &TableVersion,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableVersion, Error> {
    Ok(match version {
        TableVersion::ForSystemTimeAsOf(expr) => {
            TableVersion::ForSystemTimeAsOf(D::translate_expr(expr, schema, options, emit)?)
        }
        TableVersion::TimestampAsOf(expr) => {
            TableVersion::TimestampAsOf(D::translate_expr(expr, schema, options, emit)?)
        }
        TableVersion::VersionAsOf(expr) => {
            TableVersion::VersionAsOf(D::translate_expr(expr, schema, options, emit)?)
        }
        TableVersion::Function(expr) => {
            TableVersion::Function(D::translate_expr(expr, schema, options, emit)?)
        }
        TableVersion::Changes { changes, at, end } => {
            TableVersion::Changes {
                changes: D::translate_expr(changes, schema, options, emit)?,
                at: D::translate_expr(at, schema, options, emit)?,
                end: end
                    .as_ref()
                    .map(|e| D::translate_expr(e, schema, options, emit))
                    .transpose()?,
            }
        }
    })
}

fn translate_table_sample_quantity<D: TranslationDirection>(
    quantity: &TableSampleQuantity,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableSampleQuantity, Error> {
    Ok(TableSampleQuantity {
        parenthesized: quantity.parenthesized,
        value: D::translate_expr(&quantity.value, schema, options, emit)?,
        unit: quantity.unit,
    })
}

fn translate_table_sample_bucket<D: TranslationDirection>(
    bucket: &TableSampleBucket,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableSampleBucket, Error> {
    Ok(TableSampleBucket {
        bucket: bucket.bucket.clone(),
        total: bucket.total.clone(),
        on: bucket
            .on
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options, emit))
            .transpose()?,
    })
}

fn translate_table_sample<D: TranslationDirection>(
    sample: &TableSample,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableSample, Error> {
    Ok(TableSample {
        modifier: sample.modifier,
        name: sample.name,
        quantity: sample
            .quantity
            .as_ref()
            .map(|quantity| translate_table_sample_quantity::<D>(quantity, schema, options, emit))
            .transpose()?,
        seed: sample.seed.clone(),
        bucket: sample
            .bucket
            .as_ref()
            .map(|bucket| translate_table_sample_bucket::<D>(bucket, schema, options, emit))
            .transpose()?,
        offset: sample
            .offset
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options, emit))
            .transpose()?,
    })
}

fn translate_table_sample_kind<D: TranslationDirection>(
    sample: &TableSampleKind,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableSampleKind, Error> {
    Ok(match sample {
        TableSampleKind::BeforeTableAlias(sample) => {
            TableSampleKind::BeforeTableAlias(Box::new(translate_table_sample::<D>(
                sample, schema, options, emit,
            )?))
        }
        TableSampleKind::AfterTableAlias(sample) => {
            TableSampleKind::AfterTableAlias(Box::new(translate_table_sample::<D>(
                sample, schema, options, emit,
            )?))
        }
    })
}

fn translate_with_fill<D: TranslationDirection>(
    with_fill: &WithFill,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<WithFill, Error> {
    Ok(WithFill {
        from: with_fill
            .from
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options, emit))
            .transpose()?,
        to: with_fill
            .to
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options, emit))
            .transpose()?,
        step: with_fill
            .step
            .as_ref()
            .map(|expr| D::translate_expr(expr, schema, options, emit))
            .transpose()?,
    })
}

pub(crate) fn translate_order_by_expr<D: TranslationDirection>(
    order_by_expr: &OrderByExpr,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<OrderByExpr, Error> {
    let options_out = if D::IS_FORWARD {
        // The two databases default oppositely, PostgreSQL ASC NULLS LAST and
        // DESC NULLS FIRST against SQLite's reverse, so an absent clause is
        // filled in rather than left out. An absent direction is ASC.
        let descending =
            matches!(order_by_expr.options.sort, Some(sqlparser::ast::OrderBySort::Desc));
        sqlparser::ast::OrderByOptions {
            sort: order_by_expr.options.sort.clone(),
            nulls_first: Some(order_by_expr.options.nulls_first.unwrap_or(descending)),
        }
    } else {
        order_by_expr.options.clone()
    };

    Ok(OrderByExpr {
        expr: D::translate_expr(&order_by_expr.expr, schema, options, emit)?,
        options: options_out,
        with_fill: order_by_expr
            .with_fill
            .as_ref()
            .map(|with_fill| translate_with_fill::<D>(with_fill, schema, options, emit))
            .transpose()?,
    })
}

fn translate_expr_with_alias<D: TranslationDirection>(
    expr_with_alias: &ExprWithAlias,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<ExprWithAlias, Error> {
    Ok(ExprWithAlias {
        expr: D::translate_expr(&expr_with_alias.expr, schema, options, emit)?,
        alias: expr_with_alias.alias.clone(),
    })
}

fn translate_pivot_value_source<D: TranslationDirection>(
    value_source: &PivotValueSource,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<PivotValueSource, Error> {
    Ok(match value_source {
        PivotValueSource::List(values) => {
            PivotValueSource::List(
                values
                    .iter()
                    .map(|value| translate_expr_with_alias::<D>(value, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        PivotValueSource::Any(order_by) => {
            PivotValueSource::Any(
                order_by
                    .iter()
                    .map(|order_by_expr| {
                        translate_order_by_expr::<D>(order_by_expr, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        PivotValueSource::Subquery(query) => {
            PivotValueSource::Subquery(Box::new(D::translate_query(query, schema, options, emit)?))
        }
    })
}

fn translate_expr_with_alias_and_order_by<D: TranslationDirection>(
    expr_with_alias_and_order_by: &ExprWithAliasAndOrderBy,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<ExprWithAliasAndOrderBy, Error> {
    Ok(ExprWithAliasAndOrderBy {
        expr: translate_expr_with_alias::<D>(
            &expr_with_alias_and_order_by.expr,
            schema,
            options,
            emit,
        )?,
        order_by: expr_with_alias_and_order_by.order_by.clone(),
    })
}

fn translate_assignment<D: TranslationDirection>(
    assignment: &Assignment,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Assignment, Error> {
    Ok(Assignment {
        target: assignment.target.clone(),
        value: D::translate_expr(&assignment.value, schema, options, emit)?,
    })
}

#[allow(clippy::too_many_lines)]
fn translate_pipe_operator<D: TranslationDirection>(
    pipe_operator: &PipeOperator,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<PipeOperator, Error> {
    Ok(match pipe_operator {
        PipeOperator::Limit { expr, offset } => {
            PipeOperator::Limit {
                expr: D::translate_expr(expr, schema, options, emit)?,
                offset: offset
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .transpose()?,
            }
        }
        PipeOperator::Where { expr } => {
            PipeOperator::Where { expr: D::translate_expr(expr, schema, options, emit)? }
        }
        PipeOperator::OrderBy { exprs } => {
            PipeOperator::OrderBy {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_order_by_expr::<D>(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Select { exprs } => {
            PipeOperator::Select {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_select_item::<D>(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Extend { exprs } => {
            PipeOperator::Extend {
                exprs: exprs
                    .iter()
                    .map(|expr| translate_select_item::<D>(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Set { assignments } => {
            PipeOperator::Set {
                assignments: assignments
                    .iter()
                    .map(|assignment| translate_assignment::<D>(assignment, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Drop { columns } => PipeOperator::Drop { columns: columns.clone() },
        PipeOperator::As { alias } => PipeOperator::As { alias: alias.clone() },
        PipeOperator::Aggregate { full_table_exprs, group_by_expr } => {
            PipeOperator::Aggregate {
                full_table_exprs: full_table_exprs
                    .iter()
                    .map(|expr| {
                        translate_expr_with_alias_and_order_by::<D>(expr, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                group_by_expr: group_by_expr
                    .iter()
                    .map(|expr| {
                        translate_expr_with_alias_and_order_by::<D>(expr, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::TableSample { sample } => {
            PipeOperator::TableSample {
                sample: Box::new(translate_table_sample::<D>(
                    sample.as_ref(),
                    schema,
                    options,
                    emit,
                )?),
            }
        }
        PipeOperator::Rename { mappings } => PipeOperator::Rename { mappings: mappings.clone() },
        PipeOperator::Union { set_quantifier, queries } => {
            PipeOperator::Union {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Intersect { set_quantifier, queries } => {
            if D::IS_FORWARD && matches!(set_quantifier, SetQuantifier::All) {
                return Err(Error::forward_refusal(
                    "INTERSECT ALL is not supported in SQLite. \
                             SQLite INTERSECT always deduplicates. \
                             Use INTERSECT without ALL for the deduplicating form."
                        .to_string(),
                ));
            }
            PipeOperator::Intersect {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Except { set_quantifier, queries } => {
            if D::IS_FORWARD && matches!(set_quantifier, SetQuantifier::All) {
                return Err(Error::forward_refusal(
                    "EXCEPT ALL is not supported in SQLite. \
                             SQLite EXCEPT always deduplicates. \
                             Use EXCEPT without ALL for the deduplicating form."
                        .to_string(),
                ));
            }
            PipeOperator::Except {
                set_quantifier: *set_quantifier,
                queries: queries
                    .iter()
                    .map(|query| D::translate_query(query, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        PipeOperator::Call { function, alias } => {
            let translated_expr =
                D::translate_expr(&Expr::Function(function.clone()), schema, options, emit)?;
            let Expr::Function(translated_function) = translated_expr else {
                return Err(semantic_refusal_for::<D>(format!(
                    "Pipe CALL translation expected function expression, got {}",
                    debug_variant_name(&translated_expr)
                )));
            };
            PipeOperator::Call { function: translated_function, alias: alias.clone() }
        }
        PipeOperator::Pivot { aggregate_functions, value_column, value_source, alias } => {
            PipeOperator::Pivot {
                aggregate_functions: aggregate_functions
                    .iter()
                    .map(|expr| translate_expr_with_alias::<D>(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                value_column: value_column.clone(),
                value_source: translate_pivot_value_source::<D>(
                    value_source,
                    schema,
                    options,
                    emit,
                )?,
                alias: alias.clone(),
            }
        }
        PipeOperator::Unpivot { value_column, name_column, unpivot_columns, alias } => {
            PipeOperator::Unpivot {
                value_column: value_column.clone(),
                name_column: name_column.clone(),
                unpivot_columns: unpivot_columns.clone(),
                alias: alias.clone(),
            }
        }
        PipeOperator::Join(join) => {
            PipeOperator::Join(translate_join::<D>(join, schema, options, emit)?)
        }
    })
}

pub(crate) fn translate_query_settings<D: TranslationDirection>(
    settings: Option<&Vec<Setting>>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Vec<Setting>>, Error> {
    settings
        .map(|settings| {
            settings
                .iter()
                .map(|setting| translate_setting::<D>(setting, schema, options, emit))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
}

pub(crate) fn translate_with_clause<D: TranslationDirection>(
    with: Option<&With>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<With>, Error> {
    with.map(|w| {
        let cte_tables = w
            .cte_tables
            .iter()
            .map(|cte| {
                Ok(sqlparser::ast::Cte {
                    alias: cte.alias.clone(),
                    query: Box::new(D::translate_query(&cte.query, schema, options, emit)?),
                    from: cte.from.clone(),
                    materialized: cte.materialized,
                    closing_paren_token: cte.closing_paren_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(With { with_token: w.with_token.clone(), recursive: w.recursive, cte_tables })
    })
    .transpose()
}

pub(crate) fn translate_order_by_clause<D: TranslationDirection>(
    order_by: Option<&OrderBy>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<OrderBy>, Error> {
    order_by
        .map(|ob| -> Result<OrderBy, Error> {
            let kind = match &ob.kind {
                OrderByKind::Expressions(exprs) => {
                    OrderByKind::Expressions(
                        exprs
                            .iter()
                            .map(|expr| translate_order_by_expr::<D>(expr, schema, options, emit))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
                OrderByKind::All(all) => OrderByKind::All(all.clone()),
            };
            Ok(OrderBy { kind, interpolate: ob.interpolate.clone() })
        })
        .transpose()
}

pub(crate) fn translate_limit_clause<D: TranslationDirection>(
    limit_clause: Option<&LimitClause>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<LimitClause>, Error> {
    limit_clause
        .map(|lc| {
            Ok(match lc {
                LimitClause::LimitOffset { limit, offset, limit_by } => {
                    LimitClause::LimitOffset {
                        limit: limit
                            .as_ref()
                            .map(|e| D::translate_expr(e, schema, options, emit))
                            .transpose()?,
                        offset: offset
                            .as_ref()
                            .map(|o| {
                                Ok::<_, Error>(sqlparser::ast::Offset {
                                    value: D::translate_expr(&o.value, schema, options, emit)?,
                                    rows: o.rows,
                                })
                            })
                            .transpose()?,
                        limit_by: limit_by
                            .iter()
                            .map(|e| D::translate_expr(e, schema, options, emit))
                            .collect::<Result<Vec<_>, _>>()?,
                    }
                }
                // PostgreSQL has no comma form. The spelling puts the offset
                // first, so `LIMIT 5, 10` is offset 5 and limit 10.
                LimitClause::OffsetCommaLimit { offset, limit } if !D::IS_FORWARD => {
                    LimitClause::LimitOffset {
                        limit: Some(D::translate_expr(limit, schema, options, emit)?),
                        offset: Some(sqlparser::ast::Offset {
                            value: D::translate_expr(offset, schema, options, emit)?,
                            rows: sqlparser::ast::OffsetRows::None,
                        }),
                        limit_by: Vec::new(),
                    }
                }
                LimitClause::OffsetCommaLimit { offset, limit } => {
                    LimitClause::OffsetCommaLimit {
                        offset: D::translate_expr(offset, schema, options, emit)?,
                        limit: D::translate_expr(limit, schema, options, emit)?,
                    }
                }
            })
        })
        .transpose()
}

pub(crate) fn translate_fetch_clause<D: TranslationDirection>(
    fetch: Option<&Fetch>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Fetch>, Error> {
    fetch
        .map(|f| {
            Ok(Fetch {
                with_ties: f.with_ties,
                percent: f.percent,
                quantity: f
                    .quantity
                    .as_ref()
                    .map(|e| D::translate_expr(e, schema, options, emit))
                    .transpose()?,
            })
        })
        .transpose()
}

pub(crate) fn translate_group_by_expr<D: TranslationDirection>(
    group_by: &GroupByExpr,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<GroupByExpr, Error> {
    Ok(match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            GroupByExpr::Expressions(
                exprs
                    .iter()
                    .map(|e| D::translate_expr(e, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                modifiers.clone(),
            )
        }
        GroupByExpr::All(all) => GroupByExpr::All(all.clone()),
    })
}

pub(crate) fn translate_window_spec<D: TranslationDirection>(
    spec: &WindowSpec,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<WindowSpec, Error> {
    Ok(WindowSpec {
        window_name: spec.window_name.clone(),
        partition_by: spec
            .partition_by
            .iter()
            .map(|e| D::translate_expr(e, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?,
        order_by: spec
            .order_by
            .iter()
            .map(|e| translate_order_by_expr::<D>(e, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?,
        window_frame: spec
            .window_frame
            .as_ref()
            .map(|frame| translate_window_frame::<D>(frame, schema, options, emit))
            .transpose()?,
    })
}

pub(crate) fn translate_window_type<D: TranslationDirection>(
    over: Option<&WindowType>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<WindowType>, Error> {
    match over {
        None => Ok(None),
        Some(WindowType::NamedWindow(name)) => Ok(Some(WindowType::NamedWindow(name.clone()))),
        Some(WindowType::WindowSpec(spec)) => {
            Ok(Some(WindowType::WindowSpec(translate_window_spec::<D>(
                spec, schema, options, emit,
            )?)))
        }
    }
}

fn translate_window_frame_bound<D: TranslationDirection>(
    bound: &WindowFrameBound,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<WindowFrameBound, Error> {
    Ok(match bound {
        WindowFrameBound::Preceding(Some(e)) => {
            WindowFrameBound::Preceding(Some(Box::new(D::translate_expr(
                e, schema, options, emit,
            )?)))
        }
        WindowFrameBound::Following(Some(e)) => {
            WindowFrameBound::Following(Some(Box::new(D::translate_expr(
                e, schema, options, emit,
            )?)))
        }
        other => other.clone(),
    })
}

fn translate_window_frame<D: TranslationDirection>(
    frame: &WindowFrame,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<WindowFrame, Error> {
    Ok(WindowFrame {
        units: frame.units,
        start_bound: translate_window_frame_bound::<D>(&frame.start_bound, schema, options, emit)?,
        end_bound: frame
            .end_bound
            .as_ref()
            .map(|b| translate_window_frame_bound::<D>(b, schema, options, emit))
            .transpose()?,
    })
}

/// Translate all [`FunctionArgumentClause`] items, recursively translating
/// any [`Expr`] payloads they contain.
pub(crate) fn translate_function_argument_clauses<D: TranslationDirection>(
    clauses: &[FunctionArgumentClause],
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<FunctionArgumentClause>, Error> {
    clauses
        .iter()
        .map(|clause| translate_function_argument_clause::<D>(clause, schema, options, emit))
        .collect()
}

fn translate_function_argument_clause<D: TranslationDirection>(
    clause: &FunctionArgumentClause,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<FunctionArgumentClause, Error> {
    Ok(match clause {
        FunctionArgumentClause::OrderBy(order_by_exprs) => {
            FunctionArgumentClause::OrderBy(
                order_by_exprs
                    .iter()
                    .map(|e| translate_order_by_expr::<D>(e, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        FunctionArgumentClause::Limit(e) => {
            FunctionArgumentClause::Limit(D::translate_expr(e, schema, options, emit)?)
        }
        FunctionArgumentClause::Having(HavingBound(kind, e)) => {
            FunctionArgumentClause::Having(HavingBound(
                *kind,
                D::translate_expr(e, schema, options, emit)?,
            ))
        }
        FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate { filler, with_count }) => {
            FunctionArgumentClause::OnOverflow(ListAggOnOverflow::Truncate {
                filler: filler
                    .as_ref()
                    .map(|e| D::translate_expr(e, schema, options, emit).map(Box::new))
                    .transpose()?,
                with_count: *with_count,
            })
        }
        other => other.clone(),
    })
}

pub(crate) fn translate_named_windows<D: TranslationDirection>(
    named_windows: &[NamedWindowDefinition],
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<NamedWindowDefinition>, Error> {
    named_windows
        .iter()
        .map(|nwd| {
            let translated_expr = match &nwd.1 {
                NamedWindowExpr::NamedWindow(ident) => NamedWindowExpr::NamedWindow(ident.clone()),
                NamedWindowExpr::WindowSpec(spec) => {
                    NamedWindowExpr::WindowSpec(translate_window_spec::<D>(
                        spec, schema, options, emit,
                    )?)
                }
            };
            Ok(NamedWindowDefinition(nwd.0.clone(), translated_expr))
        })
        .collect()
}

pub(crate) fn translate_values_rows<D: TranslationDirection>(
    values: &Values,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Values, Error> {
    Ok(Values {
        explicit_row: values.explicit_row,
        rows: values
            .rows
            .iter()
            .map(|row| -> Result<sqlparser::ast::Parens<Vec<Expr>>, Error> {
                let translated = row
                    .content
                    .iter()
                    .map(|expr| {
                        if D::IS_FORWARD && is_default_keyword(expr) {
                            return Err(default_outside_an_insert_error());
                        }
                        D::translate_expr(expr, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(sqlparser::ast::Parens {
                    opening_token: row.opening_token.clone(),
                    content: translated,
                    closing_token: row.closing_token.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        value_keyword: values.value_keyword,
    })
}

/// Maps all tables in a [`FromTable`] using a caller-provided mapper.
pub(crate) fn map_from_table<E, F>(from: &FromTable, mut mapper: F) -> Result<FromTable, E>
where
    F: FnMut(&TableWithJoins) -> Result<TableWithJoins, E>,
{
    match from {
        FromTable::WithFromKeyword(tables) => {
            Ok(FromTable::WithFromKeyword(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
        FromTable::WithoutKeyword(tables) => {
            Ok(FromTable::WithoutKeyword(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

/// Maps all table lists in an [`UpdateTableFromKind`] using a caller-provided
/// mapper.
pub(crate) fn map_update_table_from_kind<E, F>(
    from: &UpdateTableFromKind,
    mut mapper: F,
) -> Result<UpdateTableFromKind, E>
where
    F: FnMut(&TableWithJoins) -> Result<TableWithJoins, E>,
{
    match from {
        UpdateTableFromKind::BeforeSet(tables) => {
            Ok(UpdateTableFromKind::BeforeSet(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
        UpdateTableFromKind::AfterSet(tables) => {
            Ok(UpdateTableFromKind::AfterSet(
                tables.iter().map(&mut mapper).collect::<Result<Vec<_>, _>>()?,
            ))
        }
    }
}

/// Shared UPDATE translation. Forward rejects joins on the target table.
pub(crate) fn translate_update<D: TranslationDirection>(
    update: &sqlparser::ast::Update,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::Update, Error> {
    if D::IS_FORWARD && !update.table.joins.is_empty() {
        return Err(Error::forward_refusal(
            "UPDATE with joins on the target table is not supported in SQLite. \
             Use UPDATE ... FROM ... instead."
                .to_string(),
        ));
    }

    // Best-effort: falls back to passthrough for unknown tables (CTEs, etc.).
    // Forward-only. Reverse receives an already-rewritten input, and it never
    // unwraps or unscales, so rewriting here would put the two directions out
    // of step rather than into it.
    let (rewrites, target_table) = if D::IS_FORWARD {
        match &update.table.relation {
            TableFactor::Table { name, .. } => {
                match resolve_translation_table(schema, name) {
                    Ok(Some(table)) => {
                        (ColumnRewrites::of_table(table, schema, D::config(options)), Some(table))
                    }
                    _ => (ColumnRewrites::default(), None),
                }
            }
            _ => (ColumnRewrites::default(), None),
        }
    } else {
        (ColumnRewrites::default(), None)
    };

    let assignments = update
        .assignments
        .iter()
        .map(|a| {
            // PostgreSQL stores the declared default for `SET col = DEFAULT`,
            // so the keyword is substituted before translation, while the
            // default is still the raw PostgreSQL expression.
            let substituted = match target_table {
                Some(table) => {
                    substituted_assignment_default(
                        &a.target,
                        &a.value,
                        table,
                        schema,
                        required_forward_context::<D>(options),
                        emit,
                    )?
                }
                None => None,
            };
            if D::IS_FORWARD && substituted.is_none() && carries_default_keyword(&a.value) {
                return Err(default_outside_an_insert_error());
            }
            let source = substituted.as_ref().unwrap_or(&a.value);
            let value = D::translate_expr(source, schema, options, emit)?;
            Ok(Assignment {
                target: a.target.clone(),
                value: rewrites.finish_assignment(&a.target, value, D::config(options))?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let selection = update
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;

    let from = update
        .from
        .as_ref()
        .map(|f| {
            map_update_table_from_kind(f, |table| {
                translate_table_with_joins::<D>(table, schema, options, emit)
            })
        })
        .transpose()?;

    let returning = translate_returning::<D>(update.returning.as_ref(), schema, options, emit)?;
    let limit = update
        .limit
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;

    let translated = sqlparser::ast::Update {
        update_token: update.update_token.clone(),
        optimizer_hints: update.optimizer_hints.clone(),
        table: translate_table_with_joins::<D>(&update.table, schema, options, emit)?,
        assignments,
        from,
        selection,
        returning,
        output: update.output.clone(),
        or: update.or,
        order_by: update.order_by.clone(),
        limit,
    };

    // Route ST_* WHERE predicates through the rtree shadow via IN-subquery.
    // Single-target-table only. UPDATE ... FROM and joined targets pass
    // through.
    if D::IS_FORWARD
        && let Some(rewritten) = crate::impls::translator_impls::postgis::try_rewrite_spatial_update(
            &translated,
            required_forward_context::<D>(options),
        )
    {
        return Ok(rewritten);
    }
    Ok(translated)
}

/// Shared DISTINCT translation. Forward rejects `DISTINCT ON` because SQLite
/// does not support it. Reverse translates `DISTINCT ON` expressions.
pub(crate) fn translate_distinct_shared<D: TranslationDirection>(
    distinct: Option<&sqlparser::ast::Distinct>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<sqlparser::ast::Distinct>, Error> {
    distinct
        .map(|d| {
            Ok(match d {
                sqlparser::ast::Distinct::On(exprs) => {
                    if D::IS_FORWARD {
                        return Err(Error::forward_refusal(
                            "DISTINCT ON is not supported in SQLite".to_string(),
                        ));
                    }
                    sqlparser::ast::Distinct::On(
                        exprs
                            .iter()
                            .map(|e| D::translate_expr(e, schema, options, emit))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                }
                sqlparser::ast::Distinct::Distinct => sqlparser::ast::Distinct::Distinct,
                sqlparser::ast::Distinct::All => sqlparser::ast::Distinct::All,
            })
        })
        .transpose()
}

/// Shared TOP translation. Forward clones as-is. Reverse translates quantity.
pub(crate) fn translate_top_shared<D: TranslationDirection>(
    top: Option<&sqlparser::ast::Top>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<sqlparser::ast::Top>, Error> {
    top.map(|t| {
        if D::IS_FORWARD {
            return Ok(t.clone());
        }
        let quantity = t
            .quantity
            .as_ref()
            .map(|q| -> Result<sqlparser::ast::TopQuantity, Error> {
                match q {
                    sqlparser::ast::TopQuantity::Expr(expr) => {
                        Ok(sqlparser::ast::TopQuantity::Expr(D::translate_expr(
                            expr, schema, options, emit,
                        )?))
                    }
                    sqlparser::ast::TopQuantity::Constant(c) => {
                        Ok(sqlparser::ast::TopQuantity::Constant(*c))
                    }
                }
            })
            .transpose()?;
        Ok(sqlparser::ast::Top { with_ties: t.with_ties, percent: t.percent, quantity })
    })
    .transpose()
}

/// Refuses the SELECT clauses that exist in neither PostgreSQL nor SQLite.
///
/// sqlparser's visitor accepts these dialect extensions on the way in, and
/// every one of them used to translate through into SQL SQLite refuses with a
/// syntax error, measured while fixing R122. Each message names the clause and
/// its home dialect. The empty fields in the rebuilt `Select` below are this
/// guard's postcondition.
fn reject_foreign_select_clauses<D: TranslationDirection>(
    select: &sqlparser::ast::Select,
) -> Result<(), Error> {
    if !select.lateral_views.is_empty() {
        return Err(unsupported_source_syntax_for::<D>(
            "LATERAL VIEW is HiveQL, and neither PostgreSQL nor SQLite has the clause. \
         PostgreSQL spells lateral iteration as a FROM item, `FROM t, LATERAL (...)`."
                .to_string(),
        ));
    }
    if !select.cluster_by.is_empty() {
        return Err(unsupported_source_syntax_for::<D>(
            "CLUSTER BY is HiveQL, and neither PostgreSQL nor SQLite has the clause. \
         Use ORDER BY."
                .to_string(),
        ));
    }
    if !select.distribute_by.is_empty() {
        return Err(unsupported_source_syntax_for::<D>(
            "DISTRIBUTE BY is HiveQL, and neither PostgreSQL nor SQLite has the clause."
                .to_string(),
        ));
    }
    if !select.sort_by.is_empty() {
        return Err(unsupported_source_syntax_for::<D>(
            "SORT BY is HiveQL, and neither PostgreSQL nor SQLite has the clause. \
         Use ORDER BY."
                .to_string(),
        ));
    }
    if select.qualify.is_some() {
        return Err(unsupported_source_syntax_for::<D>(
            "QUALIFY is Snowflake and Teradata grammar, and neither PostgreSQL nor SQLite has \
         the clause. Filter window function results in an outer query's WHERE."
                .to_string(),
        ));
    }
    if !select.connect_by.is_empty() {
        return Err(unsupported_source_syntax_for::<D>(
            "CONNECT BY is Oracle grammar, and neither PostgreSQL nor SQLite has the clause. \
         Use a recursive CTE, WITH RECURSIVE, for hierarchical queries."
                .to_string(),
        ));
    }
    Ok(())
}

/// Shared SELECT translation used by both forward and reverse paths.
pub(crate) fn translate_select_shared<D: TranslationDirection>(
    select: &sqlparser::ast::Select,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::Select, Error> {
    let from = select
        .from
        .iter()
        .map(|twj| translate_table_with_joins::<D>(twj, schema, options, emit))
        .collect::<Result<Vec<_>, _>>()?;
    translate_select_with_from::<D>(select, schema, options, emit, from)
}

pub(crate) fn translate_select_forward(
    select: &sqlparser::ast::Select,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::Select, Error> {
    use crate::impls::translator_impls::Forward;

    let from = select
        .from
        .iter()
        .map(|twj| translate_table_with_joins::<Forward>(twj, schema, options, emit))
        .collect::<Result<Vec<_>, _>>()?;
    let scope_query =
        scope_query_after_factor_rewrites(select, &from, options.cte_clause().cloned());
    if let Some(scope_query) = scope_query {
        let scope = sql_traits::structs::ColumnScope::from_query(&scope_query, schema)?;
        let scoped = options.with_scope(&scope);
        return translate_select_with_from::<Forward>(select, schema, &scoped, emit, from);
    }
    translate_select_with_from::<Forward>(select, schema, options, emit, from)
}

fn translate_select_with_from<D: TranslationDirection>(
    select: &sqlparser::ast::Select,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
    from: Vec<sqlparser::ast::TableWithJoins>,
) -> Result<sqlparser::ast::Select, Error> {
    reject_foreign_select_clauses::<D>(select)?;
    let selection = select
        .selection
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;
    let having = select
        .having
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;
    let projection = select
        .projection
        .iter()
        .map(|item| translate_select_item::<D>(item, schema, options, emit))
        .collect::<Result<Vec<_>, _>>()?;
    let prewhere = select
        .prewhere
        .as_ref()
        .map(|expr| D::translate_expr(expr, schema, options, emit))
        .transpose()?;

    let translated = sqlparser::ast::Select {
        select_token: select.select_token.clone(),
        distinct: translate_distinct_shared::<D>(select.distinct.as_ref(), schema, options, emit)?,
        top: translate_top_shared::<D>(select.top.as_ref(), schema, options, emit)?,
        top_before_distinct: select.top_before_distinct,
        projection,
        into: select.into.clone(),
        from,
        lateral_views: Vec::new(),
        prewhere,
        selection,
        group_by: translate_group_by_expr::<D>(&select.group_by, schema, options, emit)?,
        cluster_by: Vec::new(),
        distribute_by: Vec::new(),
        sort_by: Vec::new(),
        having,
        named_window: translate_named_windows::<D>(&select.named_window, schema, options, emit)?,
        qualify: None,
        window_before_qualify: select.window_before_qualify,
        value_table_mode: select.value_table_mode,
        connect_by: Vec::new(),
        flavor: select.flavor,
        exclude: select.exclude.clone(),
        optimizer_hints: select.optimizer_hints.clone(),
        select_modifiers: select.select_modifiers.clone(),
    };

    // Hooked here so DISTINCT ON and GROUPING SETS rewrites that call this
    // helper directly also receive spatial rewriting.
    if D::IS_FORWARD
        && let Some(rewritten) = crate::impls::translator_impls::postgis::try_rewrite_spatial_select(
            &translated,
            required_forward_context::<D>(options),
        )
    {
        return Ok(rewritten);
    }
    Ok(translated)
}

/// Shared `SetExpr` translation. Forward errors on `Table` and `Merge`.
pub(crate) fn translate_set_expr_shared<D: TranslationDirection>(
    set_expr: &sqlparser::ast::SetExpr,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::SetExpr, Error> {
    use sqlparser::ast::SetExpr;
    Ok(match set_expr {
        SetExpr::Select(select) if select.from.is_empty() => {
            SetExpr::Select(Box::new(translate_select_shared::<D>(select, schema, options, emit)?))
        }
        SetExpr::Select(select) => {
            // A SELECT's own FROM is what its column references resolve
            // against, and each arm of a set operation has its own, so the
            // scope is attached here rather than at the query around it.
            let scope_query = crate::impls::query_builder::make_query(
                D::cte_clause(options).cloned(),
                SetExpr::Select(select.clone()),
            );
            let scope_substitute = scope_query_for(&scope_query);
            let scope = sql_traits::structs::ColumnScope::from_query(
                scope_substitute.as_ref().unwrap_or(&scope_query),
                schema,
            )?;
            let scoped = D::with_scope(options, &scope);
            SetExpr::Select(Box::new(translate_select_shared::<D>(select, schema, &scoped, emit)?))
        }
        SetExpr::Query(query) => {
            SetExpr::Query(Box::new(translate_query_shared::<D>(query, schema, options, emit)?))
        }
        SetExpr::SetOperation { op, set_quantifier, left, right } => {
            if D::IS_FORWARD
                && matches!(set_quantifier, SetQuantifier::All)
                && matches!(op, SetOperator::Except | SetOperator::Intersect)
            {
                return Err(Error::forward_refusal(format!(
                    "{op} ALL is not supported in SQLite. SQLite {op} always deduplicates. \
                             Use {op} without the ALL quantifier for the deduplicating form."
                )));
            }
            SetExpr::SetOperation {
                op: *op,
                set_quantifier: *set_quantifier,
                left: Box::new(translate_set_expr_shared::<D>(left, schema, options, emit)?),
                right: Box::new(translate_set_expr_shared::<D>(right, schema, options, emit)?),
            }
        }
        SetExpr::Values(values) => {
            SetExpr::Values(translate_values_rows::<D>(values, schema, options, emit)?)
        }
        SetExpr::Insert(Statement::Insert(ins)) => {
            SetExpr::Insert(Statement::Insert(D::translate_insert(ins, schema, options, emit)?))
        }
        SetExpr::Update(Statement::Update(upd)) => {
            SetExpr::Update(Statement::Update(translate_update::<D>(upd, schema, options, emit)?))
        }
        SetExpr::Delete(Statement::Delete(del)) => {
            SetExpr::Delete(Statement::Delete(D::translate_delete(del, schema, options, emit)?))
        }
        SetExpr::Table(_) | SetExpr::Merge(_) => {
            if D::IS_FORWARD {
                return Err(Error::forward_refusal(
                    "TABLE and MERGE expressions are not supported in SQLite".to_string(),
                ));
            }
            set_expr.clone()
        }
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) => set_expr.clone(),
    })
}

/// Shared `Query` translation. Forward strips row locks and `for_clause`.
/// Callers may apply DISTINCT ON and GROUPING SETS rewrites first.
pub(crate) fn translate_query_shared<D: TranslationDirection>(
    query: &Query,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Query, Error> {
    let order_by = translate_order_by_clause::<D>(query.order_by.as_ref(), schema, options, emit)?;
    let settings = translate_query_settings::<D>(query.settings.as_ref(), schema, options, emit)?;
    let pipe_operators =
        translate_pipe_operators::<D>(&query.pipe_operators, schema, options, emit)?;
    let with = translate_with_clause::<D>(query.with.as_ref(), schema, options, emit)?;
    let limit_clause =
        translate_limit_clause::<D>(query.limit_clause.as_ref(), schema, options, emit)?;
    let fetch = translate_fetch_clause::<D>(query.fetch.as_ref(), schema, options, emit)?;

    Ok(Query {
        with,
        body: Box::new(translate_set_expr_shared::<D>(&query.body, schema, options, emit)?),
        order_by,
        limit_clause,
        fetch,
        // Forward: strip row-level locks (SQLite has no FOR UPDATE/SHARE).
        // Reverse: preserve as-is.
        locks: if D::IS_FORWARD { vec![] } else { query.locks.clone() },
        for_clause: if D::IS_FORWARD { None } else { query.for_clause.clone() },
        settings,
        format_clause: query.format_clause.clone(),
        pipe_operators,
    })
}

pub(crate) fn translate_pipe_operators<D: TranslationDirection>(
    pipe_operators: &[PipeOperator],
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<PipeOperator>, Error> {
    pipe_operators
        .iter()
        .map(|pipe_operator| translate_pipe_operator::<D>(pipe_operator, schema, options, emit))
        .collect::<Result<Vec<_>, _>>()
}

fn translate_measure<D: TranslationDirection>(
    measure: &Measure,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Measure, Error> {
    Ok(Measure {
        expr: D::translate_expr(&measure.expr, schema, options, emit)?,
        alias: measure.alias.clone(),
    })
}

fn translate_symbol_definition<D: TranslationDirection>(
    symbol: &SymbolDefinition,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<SymbolDefinition, Error> {
    Ok(SymbolDefinition {
        symbol: symbol.symbol.clone(),
        definition: D::translate_expr(&symbol.definition, schema, options, emit)?,
    })
}

#[allow(clippy::only_used_in_recursion)]
fn translate_json_table_column<D: TranslationDirection>(
    column: &sqlparser::ast::JsonTableColumn,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<sqlparser::ast::JsonTableColumn, Error> {
    Ok(match column {
        sqlparser::ast::JsonTableColumn::Named(named) => {
            sqlparser::ast::JsonTableColumn::Named(named.clone())
        }
        sqlparser::ast::JsonTableColumn::ForOrdinality(ident) => {
            sqlparser::ast::JsonTableColumn::ForOrdinality(ident.clone())
        }
        sqlparser::ast::JsonTableColumn::Nested(nested) => {
            sqlparser::ast::JsonTableColumn::Nested(sqlparser::ast::JsonTableNestedColumn {
                path: nested.path.clone(),
                columns: nested
                    .columns
                    .iter()
                    .map(|column| translate_json_table_column::<D>(column, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
    })
}

fn translate_xml_passing_argument<D: TranslationDirection>(
    argument: &XmlPassingArgument,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<XmlPassingArgument, Error> {
    Ok(XmlPassingArgument {
        expr: D::translate_expr(&argument.expr, schema, options, emit)?,
        alias: argument.alias.clone(),
        by_value: argument.by_value,
    })
}

fn translate_xml_passing_clause<D: TranslationDirection>(
    passing: &XmlPassingClause,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<XmlPassingClause, Error> {
    Ok(XmlPassingClause {
        arguments: passing
            .arguments
            .iter()
            .map(|argument| translate_xml_passing_argument::<D>(argument, schema, options, emit))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn translate_xml_table_column_option<D: TranslationDirection>(
    option: &XmlTableColumnOption,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<XmlTableColumnOption, Error> {
    Ok(match option {
        XmlTableColumnOption::NamedInfo { r#type, path, default, nullable } => {
            XmlTableColumnOption::NamedInfo {
                r#type: r#type.clone(),
                path: path
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .transpose()?,
                default: default
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .transpose()?,
                nullable: *nullable,
            }
        }
        XmlTableColumnOption::ForOrdinality => XmlTableColumnOption::ForOrdinality,
    })
}

fn translate_xml_table_column<D: TranslationDirection>(
    column: &XmlTableColumn,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<XmlTableColumn, Error> {
    Ok(XmlTableColumn {
        name: column.name.clone(),
        option: translate_xml_table_column_option::<D>(&column.option, schema, options, emit)?,
    })
}

fn translate_xml_namespace_definition<D: TranslationDirection>(
    namespace: &XmlNamespaceDefinition,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<XmlNamespaceDefinition, Error> {
    Ok(XmlNamespaceDefinition {
        uri: D::translate_expr(&namespace.uri, schema, options, emit)?,
        name: namespace.name.clone(),
    })
}

pub(crate) fn translate_table_with_joins<D: TranslationDirection>(
    table_with_joins: &TableWithJoins,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableWithJoins, Error> {
    let mut translated_joins = Vec::with_capacity(table_with_joins.joins.len());
    for join in &table_with_joins.joins {
        translated_joins.push(translate_join::<D>(join, schema, options, emit)?);
    }

    Ok(TableWithJoins {
        relation: translate_table_factor::<D>(&table_with_joins.relation, schema, options, emit)?,
        joins: translated_joins,
    })
}

pub(crate) fn translate_join<D: TranslationDirection>(
    join: &Join,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Join, Error> {
    Ok(Join {
        relation: translate_table_factor::<D>(&join.relation, schema, options, emit)?,
        global: join.global,
        join_operator: translate_join_operator::<D>(&join.join_operator, schema, options, emit)?,
    })
}

/// Map a [`JoinOperator`] by applying `f_constraint` to each constraint and
/// `f_expr` to the `AsOf::match_condition`.  Replaces 16+-arm match blocks
/// in `rls.rs`, `plpgsql/translator.rs`, and this module.
pub(crate) fn map_join_operator<E>(
    op: &JoinOperator,
    f_constraint: &impl Fn(&JoinConstraint) -> Result<JoinConstraint, E>,
    f_expr: &impl Fn(&Expr) -> Result<Expr, E>,
) -> Result<JoinOperator, E> {
    Ok(match op {
        JoinOperator::Join(c) => JoinOperator::Join(f_constraint(c)?),
        JoinOperator::Inner(c) => JoinOperator::Inner(f_constraint(c)?),
        JoinOperator::Left(c) => JoinOperator::Left(f_constraint(c)?),
        JoinOperator::LeftOuter(c) => JoinOperator::LeftOuter(f_constraint(c)?),
        JoinOperator::Right(c) => JoinOperator::Right(f_constraint(c)?),
        JoinOperator::RightOuter(c) => JoinOperator::RightOuter(f_constraint(c)?),
        JoinOperator::FullOuter(c) => JoinOperator::FullOuter(f_constraint(c)?),
        JoinOperator::CrossJoin(c) => JoinOperator::CrossJoin(f_constraint(c)?),
        JoinOperator::Semi(c) => JoinOperator::Semi(f_constraint(c)?),
        JoinOperator::LeftSemi(c) => JoinOperator::LeftSemi(f_constraint(c)?),
        JoinOperator::RightSemi(c) => JoinOperator::RightSemi(f_constraint(c)?),
        JoinOperator::Anti(c) => JoinOperator::Anti(f_constraint(c)?),
        JoinOperator::LeftAnti(c) => JoinOperator::LeftAnti(f_constraint(c)?),
        JoinOperator::RightAnti(c) => JoinOperator::RightAnti(f_constraint(c)?),
        JoinOperator::StraightJoin(c) => JoinOperator::StraightJoin(f_constraint(c)?),
        JoinOperator::AsOf { constraint, match_condition } => {
            JoinOperator::AsOf {
                constraint: f_constraint(constraint)?,
                match_condition: f_expr(match_condition)?,
            }
        }
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => op.clone(),
    })
}

/// Immutable reference to the [`JoinConstraint`] inside any variant that
/// carries one. Returns `None` for `CrossApply` / `OuterApply`.
#[must_use]
pub(crate) fn join_constraint_ref(op: &JoinOperator) -> Option<&JoinConstraint> {
    match op {
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::CrossJoin(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c)
        | JoinOperator::StraightJoin(c)
        | JoinOperator::AsOf { constraint: c, .. } => Some(c),
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => None,
    }
}

/// Mutable reference to the [`JoinConstraint`] inside any variant that
/// carries one. Returns `None` for `CrossApply` / `OuterApply`.
pub(crate) fn join_constraint_mut(op: &mut JoinOperator) -> Option<&mut JoinConstraint> {
    match op {
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::CrossJoin(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c)
        | JoinOperator::StraightJoin(c)
        | JoinOperator::AsOf { constraint: c, .. } => Some(c),
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => None,
    }
}

pub(crate) fn translate_join_operator<D: TranslationDirection>(
    join_operator: &JoinOperator,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<JoinOperator, Error> {
    let emit = core::cell::RefCell::new(emit);
    map_join_operator(
        join_operator,
        &|c| translate_join_constraint::<D>(c, schema, options, &mut **emit.borrow_mut()),
        &|e| D::translate_expr(e, schema, options, &mut **emit.borrow_mut()),
    )
}

pub(crate) fn translate_join_constraint<D: TranslationDirection>(
    constraint: &JoinConstraint,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<JoinConstraint, Error> {
    Ok(match constraint {
        JoinConstraint::On(expr) => {
            JoinConstraint::On(D::translate_expr(expr, schema, options, emit)?)
        }
        JoinConstraint::Using(idents) => JoinConstraint::Using(idents.clone()),
        JoinConstraint::Natural => JoinConstraint::Natural,
        JoinConstraint::None => JoinConstraint::None,
    })
}

/// Returns `true` when a derived subquery contains no FROM clause and no
/// column references, making it safe to drop a LATERAL keyword.
///
/// SQLite has no LATERAL join. A correlated lateral cannot be expressed and
/// a derived table referencing an outer column would fail at runtime with
/// "no such column". We only drop LATERAL when the subquery is trivially
/// self-contained: its body is a plain SELECT with an empty FROM list and
/// there are no Identifier or CompoundIdentifier nodes anywhere inside it.
///
/// The pattern follows `array.rs::references_a_column`, which uses the same
/// `visit_expressions` walk to detect column references in UNNEST operands.
fn subquery_is_trivially_uncorrelated(query: &Query) -> bool {
    let from_is_empty = match query.body.as_ref() {
        SetExpr::Select(sel) => sel.from.is_empty(),
        _ => false,
    };
    if !from_is_empty {
        return false;
    }
    !visit_expressions(query, |expr| {
        if matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn translate_table_factor<D: TranslationDirection>(
    table_factor: &TableFactor,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<TableFactor, Error> {
    Ok(match table_factor {
        TableFactor::Table {
            name,
            alias,
            args,
            with_hints,
            version,
            with_ordinality,
            partitions,
            json_path,
            sample,
            index_hints,
        } => {
            // generate_series with args parses as TableFactor::Table (not
            // Function).
            if D::IS_FORWARD && args.is_some() && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            if D::IS_FORWARD && sample.is_some() {
                return Err(Error::forward_refusal(
                    "TABLESAMPLE is not supported in SQLite. \
                             Use ORDER BY random() LIMIT n as an approximation."
                        .to_string(),
                ));
            }
            if D::IS_FORWARD && *with_ordinality {
                return Err(with_ordinality_not_supported_error());
            }
            // A function used where a table goes parses as `Table` carrying
            // args, not as `Function`, which is why the generate_series guard
            // above is duplicated in both arms. Anything with arguments here is
            // therefore a set-returning function rather than a relation.
            if D::IS_FORWARD
                && let Some(args) = args.as_ref()
            {
                return crate::impls::translator_impls::array::translate_set_returning_factor(
                    name,
                    &args.args,
                    alias.as_ref(),
                    schema,
                    required_forward_context::<D>(options),
                    emit,
                );
            }

            // Coming back, a call in the row-source position is a set-returning
            // function, and the ones SQLite alone has answer rows PostgreSQL
            // cannot: `json_each` differs in both its columns and what it
            // accepts, and `json_tree` does not exist there. The expression
            // classifier never sees this position, so the reason it carries is
            // read here.
            if !D::IS_FORWARD
                && args.is_some()
                && let Some(reason) =
                    crate::impls::reverse_translator_impls::function::sqlite_only_reason(
                        &crate::impls::session_variable::function_name_lower(name),
                    )
            {
                return Err(Error::reverse_refusal(reason));
            }

            // SQLite accepts no column list on a table alias, the same
            // limitation that forces the derived shape in
            // translate_unnest_factor, so the rename happens in a projection
            // over the relation (R105).
            if D::IS_FORWARD
                && let Some(alias) = alias.as_ref().filter(|alias| !alias.columns.is_empty())
            {
                return renamed_relation_factor::<D>(name, alias, schema, options, emit);
            }
            TableFactor::Table {
                name: D::translate_object_name(name, schema, options)?,
                alias: alias.clone(),
                args: args
                    .as_ref()
                    .map(|args| translate_table_function_args::<D>(args, schema, options, emit))
                    .transpose()?,
                with_hints: with_hints
                    .iter()
                    .map(|hint| D::translate_expr(hint, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                version: version
                    .as_ref()
                    .map(|version| translate_table_version::<D>(version, schema, options, emit))
                    .transpose()?,
                with_ordinality: *with_ordinality,
                partitions: partitions.clone(),
                json_path: json_path.clone(),
                sample: sample
                    .as_ref()
                    .map(|sample| translate_table_sample_kind::<D>(sample, schema, options, emit))
                    .transpose()?,
                index_hints: index_hints.clone(),
            }
        }
        TableFactor::Derived { subquery, lateral, alias, sample } => {
            if D::IS_FORWARD {
                // SQLite grammar has no column list on a table alias.
                // The same limitation forces the derived-table shape in
                // array.rs::translate_unnest_factor.
                if alias.as_ref().is_some_and(|a| !a.columns.is_empty()) {
                    return Err(Error::forward_refusal("Table alias with a column list (AS alias(col1, col2, ...)) is not \
                                     supported in SQLite grammar. Project the column names instead, for \
                                     example: SELECT column1 AS a FROM (VALUES (1),(2)) AS v"
                        .to_string()));
                }
                // SQLite has no LATERAL join. Drop the keyword only when the
                // subquery is trivially uncorrelated (no FROM clause, no column
                // references). Any other case would fail at runtime with
                // "no such column" because the outer scope is invisible.
                if *lateral && !subquery_is_trivially_uncorrelated(subquery) {
                    return Err(Error::forward_refusal("LATERAL on a correlated subquery is not supported in SQLite. SQLite \
                                     has no LATERAL join. A correlated lateral cannot be expressed and a \
                                     derived table would fail at runtime with no such column."
                        .to_string()));
                }
                if sample.is_some() {
                    return Err(Error::forward_refusal(
                        "TABLESAMPLE is not supported in SQLite. \
                                     Use ORDER BY random() LIMIT n as an approximation."
                            .to_string(),
                    ));
                }
            }
            TableFactor::Derived {
                subquery: Box::new(D::translate_query(subquery, schema, options, emit)?),
                // Drop LATERAL; uncorrelated subqueries are safe without it and
                // correlated ones are rejected above.
                lateral: false,
                alias: alias.clone(),
                sample: sample
                    .as_ref()
                    .map(|sample| translate_table_sample_kind::<D>(sample, schema, options, emit))
                    .transpose()?,
            }
        }
        TableFactor::TableFunction { expr, alias } => {
            TableFactor::TableFunction {
                expr: D::translate_expr(expr, schema, options, emit)?,
                alias: alias.clone(),
            }
        }
        TableFactor::Function { lateral, name, args, with_ordinality, alias } => {
            if D::IS_FORWARD && is_generate_series_object_name(name) {
                return Err(generate_series_not_supported_error());
            }
            if D::IS_FORWARD && *with_ordinality {
                return Err(with_ordinality_not_supported_error());
            }
            if D::IS_FORWARD {
                return crate::impls::translator_impls::array::translate_set_returning_factor(
                    name,
                    args,
                    alias.as_ref(),
                    schema,
                    required_forward_context::<D>(options),
                    emit,
                );
            }
            TableFactor::Function {
                lateral: *lateral,
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| translate_function_arg::<D>(arg, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                with_ordinality: *with_ordinality,
                alias: alias.clone(),
            }
        }
        TableFactor::UNNEST {
            alias,
            array_exprs,
            with_offset,
            with_offset_alias,
            with_ordinality,
        } => {
            // SQLite has no UNNEST; forward translation lowers it onto
            // `json_each`. Reverse translation leaves it alone.
            if D::IS_FORWARD {
                return crate::impls::translator_impls::array::translate_unnest_factor(
                    array_exprs,
                    alias.as_ref(),
                    *with_offset,
                    *with_ordinality,
                    schema,
                    required_forward_context::<D>(options),
                    emit,
                );
            }
            TableFactor::UNNEST {
                alias: alias.clone(),
                array_exprs: array_exprs
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                with_offset: *with_offset,
                with_offset_alias: with_offset_alias.clone(),
                with_ordinality: *with_ordinality,
            }
        }
        TableFactor::JsonTable { json_expr, json_path, columns, alias } => {
            TableFactor::JsonTable {
                json_expr: D::translate_expr(json_expr, schema, options, emit)?,
                json_path: json_path.clone(),
                columns: columns
                    .iter()
                    .map(|column| translate_json_table_column::<D>(column, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::OpenJsonTable { json_expr, json_path, columns, alias } => {
            TableFactor::OpenJsonTable {
                json_expr: D::translate_expr(json_expr, schema, options, emit)?,
                json_path: json_path.clone(),
                columns: columns.clone(),
                alias: alias.clone(),
            }
        }
        TableFactor::NestedJoin { table_with_joins, alias } => {
            TableFactor::NestedJoin {
                table_with_joins: Box::new(translate_table_with_joins::<D>(
                    table_with_joins,
                    schema,
                    options,
                    emit,
                )?),
                alias: alias.clone(),
            }
        }
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            alias,
        } => {
            TableFactor::Pivot {
                table: Box::new(translate_table_factor::<D>(table, schema, options, emit)?),
                aggregate_functions: aggregate_functions
                    .iter()
                    .map(|expr_with_alias| {
                        translate_expr_with_alias::<D>(expr_with_alias, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                value_column: value_column
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                value_source: translate_pivot_value_source::<D>(
                    value_source,
                    schema,
                    options,
                    emit,
                )?,
                default_on_null: default_on_null
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .transpose()?,
                alias: alias.clone(),
            }
        }
        TableFactor::Unpivot { table, value, name, columns, null_inclusion, alias } => {
            TableFactor::Unpivot {
                table: Box::new(translate_table_factor::<D>(table, schema, options, emit)?),
                value: D::translate_expr(value, schema, options, emit)?,
                name: name.clone(),
                columns: columns
                    .iter()
                    .map(|expr_with_alias| {
                        translate_expr_with_alias::<D>(expr_with_alias, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                null_inclusion: null_inclusion.clone(),
                alias: alias.clone(),
            }
        }
        TableFactor::UnpivotExpr { .. } => {
            return Err(unsupported_source_syntax_for::<D>(
                "UNPIVOT over an expression (Redshift object unpivoting) is not supported. \
                     Neither PostgreSQL nor SQLite has this construct. Unpivot a JSON document \
                     with json_each instead."
                    .to_string(),
            ));
        }
        TableFactor::MatchRecognize {
            table,
            partition_by,
            order_by,
            measures,
            rows_per_match,
            after_match_skip,
            pattern,
            symbols,
            alias,
        } => {
            TableFactor::MatchRecognize {
                table: Box::new(translate_table_factor::<D>(table, schema, options, emit)?),
                partition_by: partition_by
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                order_by: order_by
                    .iter()
                    .map(|order_by_expr| {
                        translate_order_by_expr::<D>(order_by_expr, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                measures: measures
                    .iter()
                    .map(|measure| translate_measure::<D>(measure, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                rows_per_match: rows_per_match.clone(),
                after_match_skip: after_match_skip.clone(),
                pattern: pattern.clone(),
                symbols: symbols
                    .iter()
                    .map(|symbol| translate_symbol_definition::<D>(symbol, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::XmlTable { namespaces, row_expression, passing, columns, alias } => {
            TableFactor::XmlTable {
                namespaces: namespaces
                    .iter()
                    .map(|namespace| {
                        translate_xml_namespace_definition::<D>(namespace, schema, options, emit)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                row_expression: D::translate_expr(row_expression, schema, options, emit)?,
                passing: translate_xml_passing_clause::<D>(passing, schema, options, emit)?,
                columns: columns
                    .iter()
                    .map(|column| translate_xml_table_column::<D>(column, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                alias: alias.clone(),
            }
        }
        TableFactor::SemanticView { name, dimensions, metrics, facts, where_clause, alias } => {
            TableFactor::SemanticView {
                name: name.clone(),
                dimensions: dimensions
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                metrics: metrics
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                facts: facts
                    .iter()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .collect::<Result<Vec<_>, _>>()?,
                where_clause: where_clause
                    .as_ref()
                    .map(|expr| D::translate_expr(expr, schema, options, emit))
                    .transpose()?,
                alias: alias.clone(),
            }
        }
    })
}

/// Rewrites `FROM t AS x (a, b)` into `(SELECT id AS a, s AS b FROM t) AS x`.
///
/// PostgreSQL renames the table's leading columns positionally and keeps the
/// rest under their declared names, and it refuses a list longer than the
/// table or one carrying data types, which belong to a function returning
/// `record`. SQLite accepts no column list on a table alias, so the rename
/// happens in a projection, the shape `translate_unnest_factor` and the
/// `Derived` arm already use for the same reason. The declared idents are
/// rebuilt with their quoting so a quoted column name stays quoted in the
/// projection.
fn renamed_relation_factor<D: TranslationDirection>(
    name: &ObjectName,
    alias: &TableAlias,
    schema: &ParserDB,
    options: &D::Options<'_>,
    _emit: crate::warnings::WarningSink<'_>,
) -> Result<TableFactor, Error> {
    if let Some(typed) = alias.columns.iter().find(|column| column.data_type.is_some()) {
        return Err(Error::forward_refusal(format!(
            "FROM {name} AS {} ({} ...) carries a data type in the column alias list. \
             PostgreSQL only accepts one on a function returning record, so a file carrying \
             it on a table is not the input this crate translates. Name the columns alone.",
            alias.name, typed.name
        )));
    }

    let Some(table) = resolve_translation_table(schema, name)? else {
        return Err(Error::forward_refusal(format!(
            "FROM {name} AS {} (...) renames the columns of a relation the translation schema \
             does not declare, so the declared column list the rewrite needs is unknown. \
             Include the relation's definition in the same translation batch.",
            alias.name
        )));
    };

    let declared: Vec<Ident> = table
        .columns(schema)?
        .map(|column| {
            if column.column_name_is_quoted() {
                Ident::with_quote('"', column.column_name())
            } else {
                Ident::new(column.column_name())
            }
        })
        .collect();

    if alias.columns.len() > declared.len() {
        return Err(Error::forward_refusal(format!(
            "FROM {name} AS {} (...) names {} columns for a table that declares only {}. \
             PostgreSQL refuses the longer list too. Name at most the table's column count.",
            alias.name,
            alias.columns.len(),
            declared.len()
        )));
    }

    let projection = declared
        .into_iter()
        .enumerate()
        .map(|(position, column)| {
            match alias.columns.get(position) {
                Some(renamed) => {
                    SelectItem::ExprWithAlias {
                        expr: Expr::Identifier(column),
                        alias: renamed.name.clone(),
                    }
                }
                None => SelectItem::UnnamedExpr(Expr::Identifier(column)),
            }
        })
        .collect();

    let relation = TableFactor::Table {
        name: D::translate_object_name(name, schema, options)?,
        alias: None,
        args: None,
        with_hints: Vec::new(),
        version: None,
        with_ordinality: false,
        partitions: Vec::new(),
        json_path: None,
        sample: None,
        index_hints: Vec::new(),
    };

    Ok(TableFactor::Derived {
        lateral: false,
        subquery: Box::new(make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(
                projection,
                from_relation(relation),
                None,
            ))),
        )),
        alias: Some(TableAlias {
            explicit: true,
            name: alias.name.clone(),
            columns: Vec::new(),
            at: None,
        }),
        sample: None,
    })
}

pub(crate) fn translate_select_item<D: TranslationDirection>(
    item: &SelectItem,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<SelectItem, Error> {
    Ok(match item {
        SelectItem::UnnamedExpr(expr) => {
            SelectItem::UnnamedExpr(D::translate_expr(expr, schema, options, emit)?)
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            SelectItem::ExprWithAlias {
                expr: D::translate_expr(expr, schema, options, emit)?,
                alias: alias.clone(),
            }
        }
        other => other.clone(),
    })
}

pub(crate) fn translate_returning<D: TranslationDirection>(
    returning: Option<&Vec<SelectItem>>,
    schema: &ParserDB,
    options: &D::Options<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Option<Vec<SelectItem>>, Error> {
    match returning {
        Some(items) => {
            let mut translated = Vec::with_capacity(items.len());
            for item in items {
                translated.push(translate_select_item::<D>(item, schema, options, emit)?);
            }
            Ok(Some(translated))
        }
        None => Ok(None),
    }
}
fn semantic_refusal_for<D: TranslationDirection>(detail: impl Into<String>) -> Error {
    if D::IS_FORWARD { Error::forward_refusal(detail) } else { Error::reverse_refusal(detail) }
}

fn unsupported_source_syntax_for<D: TranslationDirection>(detail: impl Into<String>) -> Error {
    if D::IS_FORWARD {
        Error::unsupported_source_syntax(detail)
    } else {
        Error::reverse_unsupported_source_syntax(detail)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{
            Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgOperator,
            FunctionArgumentList, FunctionArguments, Ident, JoinConstraint, JoinOperator,
            ObjectName, ObjectNamePart, Query, SelectItem, SetExpr, Statement, TableFactor,
            ValueWithSpan,
        },
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        ColumnReferences, TranslationDirection, extract_columns_from_expr,
        extract_columns_from_function, translate_join, translate_join_constraint,
        translate_join_operator, translate_returning, translate_select_item,
        translate_table_factor, translate_table_with_joins,
    };
    use crate::{errors::Error, prelude::Pg2SqliteOptions};

    struct IdentityDirection;

    impl TranslationDirection for IdentityDirection {
        type Options<'a> = Pg2SqliteOptions;

        fn with_scope<'scope>(
            options: &'scope Self::Options<'_>,
            _scope: &'scope sql_traits::structs::ColumnScope<'scope, 'scope, ParserDB>,
        ) -> Self::Options<'scope> {
            options.clone()
        }

        fn config<'options>(options: &'options Self::Options<'_>) -> &'options Pg2SqliteOptions {
            options
        }

        fn translate_expr(
            expr: &Expr,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<Expr, Error> {
            Ok(expr.clone())
        }

        fn translate_query(
            query: &Query,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<Query, Error> {
            Ok(query.clone())
        }

        fn translate_insert(
            insert: &sqlparser::ast::Insert,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<sqlparser::ast::Insert, Error> {
            Ok(insert.clone())
        }

        fn translate_delete(
            delete: &sqlparser::ast::Delete,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<sqlparser::ast::Delete, Error> {
            Ok(delete.clone())
        }
    }

    struct NestingDirection;

    impl TranslationDirection for NestingDirection {
        type Options<'a> = Pg2SqliteOptions;

        fn with_scope<'scope>(
            options: &'scope Self::Options<'_>,
            _scope: &'scope sql_traits::structs::ColumnScope<'scope, 'scope, ParserDB>,
        ) -> Self::Options<'scope> {
            options.clone()
        }

        fn config<'options>(options: &'options Self::Options<'_>) -> &'options Pg2SqliteOptions {
            options
        }

        fn translate_expr(
            expr: &Expr,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<Expr, Error> {
            Ok(Expr::Nested(Box::new(expr.clone())))
        }

        fn translate_query(
            query: &Query,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<Query, Error> {
            Ok(query.clone())
        }

        fn translate_insert(
            insert: &sqlparser::ast::Insert,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<sqlparser::ast::Insert, Error> {
            Ok(insert.clone())
        }

        fn translate_delete(
            delete: &sqlparser::ast::Delete,
            _schema: &ParserDB,
            _options: &Pg2SqliteOptions,
            _emit: crate::warnings::WarningSink<'_>,
        ) -> Result<sqlparser::ast::Delete, Error> {
            Ok(delete.clone())
        }
    }

    fn empty_schema() -> ParserDB {
        ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap()
    }

    fn parse_expr(sql: &str) -> Expr {
        Parser::new(&PostgreSqlDialect {}).try_with_sql(sql).unwrap().parse_expr().unwrap()
    }

    fn parse_query(sql: &str) -> Query {
        let stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap();
        match stmts.into_iter().next().unwrap() {
            Statement::Query(query) => *query,
            other => panic!("expected query statement, got: {other:?}"),
        }
    }

    #[test]
    fn translates_join_structures_and_select_items() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query(
            "SELECT t.a AS a1 FROM t INNER JOIN u ON t.id = u.id LEFT JOIN v ON u.id = v.uid",
        );
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let translated = translate_table_with_joins::<IdentityDirection>(
            select.from.first().unwrap(),
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(translated.joins.len(), 2);

        let unnamed = SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("a")));
        let named = SelectItem::ExprWithAlias {
            expr: Expr::Identifier(sqlparser::ast::Ident::new("b")),
            alias: sqlparser::ast::Ident::new("b1"),
        };
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&unnamed, &schema, &options, &mut |_| {},)
                .unwrap(),
            SelectItem::UnnamedExpr(_)
        ));
        assert!(matches!(
            translate_select_item::<IdentityDirection>(&named, &schema, &options, &mut |_| {},)
                .unwrap(),
            SelectItem::ExprWithAlias { .. }
        ));
    }

    #[test]
    fn translates_all_join_operator_variants() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let on = JoinConstraint::On(Expr::Value(ValueWithSpan::from(
            sqlparser::ast::Value::Boolean(true),
        )));

        let operators = vec![
            JoinOperator::Join(on.clone()),
            JoinOperator::Inner(on.clone()),
            JoinOperator::Left(on.clone()),
            JoinOperator::LeftOuter(on.clone()),
            JoinOperator::Right(on.clone()),
            JoinOperator::RightOuter(on.clone()),
            JoinOperator::FullOuter(on.clone()),
            JoinOperator::CrossJoin(on.clone()),
            JoinOperator::Semi(on.clone()),
            JoinOperator::LeftSemi(on.clone()),
            JoinOperator::RightSemi(on.clone()),
            JoinOperator::Anti(on.clone()),
            JoinOperator::LeftAnti(on.clone()),
            JoinOperator::RightAnti(on.clone()),
            JoinOperator::AsOf {
                constraint: on.clone(),
                match_condition: Expr::Value(ValueWithSpan::from(sqlparser::ast::Value::Number(
                    "1".to_string(),
                    false,
                ))),
            },
            JoinOperator::StraightJoin(on.clone()),
            JoinOperator::CrossApply,
            JoinOperator::OuterApply,
        ];

        for op in &operators {
            let _ =
                translate_join_operator::<IdentityDirection>(op, &schema, &options, &mut |_| {})
                    .unwrap();
        }

        let _ = translate_join_constraint::<IdentityDirection>(&on, &schema, &options, &mut |_| {})
            .unwrap();
    }

    #[test]
    fn translates_table_factor_and_returning() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM (SELECT 1) AS q");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };

        let derived = &select.from[0].relation;
        let _ =
            translate_table_factor::<IdentityDirection>(derived, &schema, &options, &mut |_| {})
                .unwrap();

        let nested_query = parse_query("SELECT * FROM (t JOIN u ON t.id = u.id) AS z");
        let sqlparser::ast::SetExpr::Select(nested_select) = nested_query.body.as_ref() else {
            panic!("expected select");
        };
        let nested_factor = &nested_select.from[0].relation;
        if let TableFactor::NestedJoin { .. } = nested_factor {
            let _ = translate_table_factor::<IdentityDirection>(
                nested_factor,
                &schema,
                &options,
                &mut |_| {},
            )
            .unwrap();
        }

        let joined_query = parse_query("SELECT * FROM t INNER JOIN u ON t.id = u.id");
        let SetExpr::Select(joined_select) = joined_query.body.as_ref() else {
            panic!("expected select");
        };
        let manual_nested = TableFactor::NestedJoin {
            table_with_joins: Box::new(joined_select.from[0].clone()),
            alias: None,
        };
        let translated_manual = translate_table_factor::<IdentityDirection>(
            &manual_nested,
            &schema,
            &options,
            &mut |_| {},
        )
        .unwrap();
        assert!(matches!(translated_manual, TableFactor::NestedJoin { .. }));

        let returning_items = vec![
            SelectItem::UnnamedExpr(Expr::Identifier(sqlparser::ast::Ident::new("id"))),
            SelectItem::ExprWithAlias {
                expr: Expr::Identifier(sqlparser::ast::Ident::new("name")),
                alias: sqlparser::ast::Ident::new("n"),
            },
        ];
        assert_eq!(
            translate_returning::<IdentityDirection>(
                Some(&returning_items),
                &schema,
                &options,
                &mut |_| {},
            )
            .unwrap()
            .unwrap()
            .len(),
            2
        );
        assert!(
            translate_returning::<IdentityDirection>(None, &schema, &options, &mut |_| {},)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn translate_join_preserves_global_flag() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let query = parse_query("SELECT * FROM t INNER JOIN u ON t.id = u.id");
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select");
        };
        let mut join = select.from[0].joins[0].clone();
        join.global = true;
        let translated =
            translate_join::<IdentityDirection>(&join, &schema, &options, &mut |_| {}).unwrap();
        assert!(translated.global);
    }

    #[test]
    fn asof_and_expr_alias_apply_expr_translation_direction() {
        let schema = empty_schema();
        let options = Pg2SqliteOptions::default();
        let as_of = JoinOperator::AsOf {
            constraint: JoinConstraint::On(parse_expr("t.id = u.id")),
            match_condition: parse_expr("t.id > u.id"),
        };
        let translated_as_of =
            translate_join_operator::<NestingDirection>(&as_of, &schema, &options, &mut |_| {})
                .unwrap();
        let JoinOperator::AsOf { match_condition, .. } = translated_as_of else {
            panic!("expected AS OF join");
        };
        assert!(matches!(match_condition, Expr::Nested(_)));

        let alias_item = SelectItem::ExprWithAlias {
            expr: parse_expr("a"),
            alias: sqlparser::ast::Ident::new("a1"),
        };
        let translated_alias =
            translate_select_item::<NestingDirection>(&alias_item, &schema, &options, &mut |_| {})
                .unwrap();
        let SelectItem::ExprWithAlias { expr, .. } = translated_alias else {
            panic!("expected alias expression");
        };
        assert!(matches!(expr, Expr::Nested(_)));
    }

    #[test]
    fn extract_helpers_cover_named_and_non_expr_argument_shapes() {
        let named_func = Function {
            name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("f"))]),
            uses_odbc_syntax: false,
            args: FunctionArguments::List(FunctionArgumentList {
                duplicate_treatment: None,
                args: vec![
                    FunctionArg::Named {
                        name: Ident::new("x"),
                        arg: FunctionArgExpr::Expr(parse_expr("tbl.col")),
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
        };
        let cols = extract_columns_from_function(&named_func);
        assert_eq!(cols, ColumnReferences::Complete(vec!["col".to_string()]));

        let none_args_func = Function { args: FunctionArguments::None, ..named_func.clone() };
        assert_eq!(
            extract_columns_from_function(&none_args_func),
            ColumnReferences::Complete(Vec::new())
        );

        assert_eq!(
            extract_columns_from_expr(&Expr::CompoundIdentifier(Vec::new())),
            ColumnReferences::Complete(Vec::new())
        );
        assert_eq!(
            extract_columns_from_expr(&Expr::Nested(Box::new(parse_expr("a + b")))),
            ColumnReferences::Complete(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            extract_columns_from_expr(&Expr::Cast {
                expr: Box::new(parse_expr("payload")),
                data_type: sqlparser::ast::DataType::Text,
                format: None,
                kind: sqlparser::ast::CastKind::Cast,
            }),
            ColumnReferences::Complete(vec!["payload".to_string()])
        );
        assert_eq!(
            extract_columns_from_expr(&parse_expr(
                "CASE WHEN enabled THEN -assigned_id ELSE fallback END"
            )),
            ColumnReferences::Complete(vec![
                "enabled".to_string(),
                "assigned_id".to_string(),
                "fallback".to_string()
            ])
        );
        assert_eq!(
            extract_columns_from_expr(&parse_expr("EXISTS (SELECT outer_id FROM nested)")),
            ColumnReferences::Unknown
        );
        assert_eq!(
            extract_columns_from_expr(&Expr::Function(named_func)),
            ColumnReferences::Complete(vec!["col".to_string()])
        );
    }
}
