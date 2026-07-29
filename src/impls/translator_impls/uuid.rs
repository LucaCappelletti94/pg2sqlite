//! Translation glue for `UUID` columns under
//! [`crate::traits::UuidRepresentation::Blob`]. Mirrors the pgvector
//! text-literal wrap in [`super::vector`] but for `DataType::Uuid`:
//! INSERT/UPDATE values that are bare text literals get rewritten to a
//! binary-conversion call before they reach the BLOB STRICT column.
//!
//! The default conversion expression is the pure-SQLite shape
//! `unhex(replace(literal, '-', ''))` so callers do not need to register
//! a custom UDF. When
//! [`crate::traits::TranslationOptions::with_uuid_text_to_blob_function_name`]
//! is configured, the translator instead emits a call to that UDF.

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

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    BinaryOperator, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, Value, ValueWithSpan,
};

use crate::{
    prelude::Pg2SqliteOptions,
    traits::{TranslationOptions, UuidRepresentation},
};

/// True when the data type is the PostgreSQL `UUID` builtin.
#[must_use]
pub(crate) fn is_uuid_data_type(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Uuid)
}

/// True when the configured representation maps a UUID column to a
/// SQLite `BLOB STRICT` column.
#[must_use]
pub(crate) fn is_blob_uuid_representation(options: &Pg2SqliteOptions) -> bool {
    matches!(options.get_uuid_representation(), Some(UuidRepresentation::Blob))
}

/// Collect every UUID column name on the resolved schema table,
/// preserving column ordinal order. Returns an empty `Vec` when the
/// table has no UUID columns.
#[must_use]
pub(crate) fn uuid_columns_of_table(
    table: &<ParserDB as DatabaseLike>::Table,
    schema: &ParserDB,
) -> Vec<String> {
    table
        .columns(schema)
        .filter_map(|col| {
            let dt = &col.attribute().data_type;
            if is_uuid_data_type(dt) { Some(col.column_name().to_string()) } else { None }
        })
        .collect()
}

/// Build the expression that converts `arg` (a text UUID literal or any
/// other expression producing a textual UUID) into a 16-byte BLOB. When
/// the caller configured a UDF name, emits `<udf_name>(arg)`. Otherwise
/// emits the pure-SQLite shape `unhex(replace(arg, '-', ''))`.
#[must_use]
pub(crate) fn make_uuid_conversion_call(arg: Expr, options: &Pg2SqliteOptions) -> Expr {
    if let Some(udf) = options.get_uuid_text_to_blob_function_name() {
        return single_arg_function(udf, arg);
    }
    let dash_literal = string_literal_expr("-");
    let empty_literal = string_literal_expr("");
    let replace_call = three_arg_function("replace", arg, dash_literal, empty_literal);
    single_arg_function("unhex", replace_call)
}

/// If `expr` is a single-quoted string literal, wrap it with the
/// configured UUID text-to-blob conversion. Other shapes pass through
/// untouched. NULL, DEFAULT, identifiers, casts, and existing function
/// calls (including a pre-wrapped `unhex(replace(...))`) are left
/// alone, so this helper is idempotent within a single translation
/// pass.
#[must_use]
pub(crate) fn maybe_wrap_text_uuid_literal(expr: Expr, options: &Pg2SqliteOptions) -> Expr {
    if matches!(&expr, Expr::Value(ValueWithSpan { value: Value::SingleQuotedString(_), .. })) {
        make_uuid_conversion_call(expr, options)
    } else {
        expr
    }
}

/// Build a column-level `CHECK (length(<col>) = 16)` ColumnOption. The
/// translator attaches this to every UUID-Blob column so parameterised
/// callers that skip the text-wrap path (e.g. a Rust app binding a
/// stringly-typed value) still get rejected by SQLite at insert time
/// instead of silently storing a non-UUID BLOB.
#[must_use]
pub(crate) fn uuid_blob_length_check_expr(column_name: &Ident) -> Expr {
    let length_call = single_arg_function("length", Expr::Identifier(column_name.clone()));
    Expr::BinaryOp {
        left: Box::new(length_call),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Value(ValueWithSpan {
            value: Value::Number("16".to_string(), false),
            span: sqlparser::tokenizer::Span::empty(),
        })),
    }
}

fn single_arg_function(name: &str, arg: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(arg))],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    })
}

fn three_arg_function(name: &str, a: Expr, b: Expr, c: Expr) -> Expr {
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        uses_odbc_syntax: false,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                FunctionArg::Unnamed(FunctionArgExpr::Expr(a)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(b)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(c)),
            ],
            clauses: vec![],
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        parameters: FunctionArguments::None,
    })
}

fn string_literal_expr(s: &str) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::SingleQuotedString(s.to_string()),
        span: sqlparser::tokenizer::Span::empty(),
    })
}
