//! Undoing a storage wrapper needs the column's PostgreSQL type, not the
//! wrapper's name.
//!
//! `unhex`, `hex`, `json_array` and `json_array_length` all appear in a replica
//! over a column whose PostgreSQL type is not the storage type: a `uuid` held
//! as a blob, an `integer[]` held as JSON text. Mapping each name to its
//! `bytea` or `json` counterpart named the storage type instead of the
//! declared one, and the server refused the statement with `operator does not
//! exist: uuid = bytea`, `cannot cast type uuid to bytea`, `column "xs" is of
//! type integer[] but expression is of type json` and `function
//! json_array_length(integer[]) does not exist`, all measured on 17.3.
//!
//! Two shapes need two answers. `hex` and `json_array_length` carry the column
//! as their argument, so the arms in `function.rs` read its declared type
//! themselves. `unhex` and `json_array` carry only a value, and the type comes
//! from the position they sit in: the other side of a comparison, the column an
//! `INSERT` fills, the column an `UPDATE` assigns. Those positions call
//! [`retype_for_declared`] with the type they already know.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{boxed::Box, format, string::ToString, vec, vec::Vec};

use sql_traits::structs::ParserDB;
use sqlparser::ast::{CastKind, DataType, Expr, FunctionArguments, ObjectNamePart};

use crate::{errors::Error, impls::shared_helpers::declared_in_scope};

/// The PostgreSQL type `expr` is declared with, when the schema names one.
///
/// `None` where `expr` names no column, since a literal or a computed value
/// has no declaration to undo a storage wrapper by.
///
/// # Errors
///
/// Propagates the unresolved reference the scope answers for a column no
/// relation in it declares. That case used to read as no declaration, which
/// reversed `unhex` by its storage type and emitted `decode(..., 'hex')`
/// against a column the server may hold as a uuid, where it answers
/// `operator does not exist: uuid = bytea`.
pub(crate) fn declared_data_type(
    expr: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Option<DataType>, Error> {
    declared_in_scope(
        expr,
        schema,
        options,
        |column| Some(column.data_type.clone()),
        declared_data_type,
    )
}

/// True when undoing `value` needs the column's declared type, so reading it
/// wrongly changes what the statement means.
///
/// A hex decode is a `bytea`, a `uuid` held as a blob, or neither, and a JSON
/// array is an array column or a document. Everything else reverses the same
/// whatever the column is declared as, so it never asks.
pub(crate) fn needs_declared_type(value: &Expr) -> bool {
    hex_decoded_argument(value).is_some() || is_json_array_construction(value)
}

/// True when `value` is the JSON array construction an array column's value
/// reverses from.
fn is_json_array_construction(value: &Expr) -> bool {
    let Expr::Function(function) = value else { return false };
    function_is_named(function, "json_build_array")
        || function_is_named(function, "jsonb_build_array")
}

/// Rewrites an already-reversed value for the column it lands in.
///
/// A `decode(x, 'hex')` landing in a `uuid` column becomes `CAST(x AS UUID)`,
/// which PostgreSQL accepts for the 32 hex digits SQLite's `unhex` took, and a
/// `json_build_array(...)` landing in an array column becomes an `ARRAY[...]`
/// constructor. Anything else is returned untouched, including the `bytea` and
/// `json` columns the original mapping was right for.
///
/// # Errors
///
/// Returns [`Error::TranslationRefusal`] for a hex blob landing in a `uuid`
/// column the replica holds as text, where SQLite compares a blob with text and
/// never matches, so no PostgreSQL form answers the same.
pub(crate) fn retype_for_declared(
    value: Expr,
    declared: &DataType,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    match declared {
        DataType::Uuid => retype_hex_blob_as_uuid(value, options),
        DataType::Array(_) => Ok(retype_json_array_as_array(value, declared)),
        _ => Ok(value),
    }
}

/// Reads the declared type of `peer` and retypes `value` for it.
///
/// The type is read only for a value whose meaning depends on it, so a
/// reference the scope cannot answer costs nothing where the answer would
/// have changed nothing.
///
/// # Errors
///
/// Propagates the refusal [`retype_for_declared`] makes, and the unresolved
/// reference [`declared_data_type`] answers for a value that does depend on
/// the declaration.
pub(crate) fn retype_against_peer(
    value: Expr,
    peer: &Expr,
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    if !needs_declared_type(&value) {
        return Ok(value);
    }
    match declared_data_type(peer, schema, options)? {
        Some(declared) => retype_for_declared(value, &declared, options),
        None => Ok(value),
    }
}

/// `decode(x, 'hex')` -> `CAST(x AS UUID)` for a `uuid` column.
fn retype_hex_blob_as_uuid(
    value: Expr,
    options: &crate::options::TranslationContext<'_>,
) -> Result<Expr, Error> {
    let Some(hex_argument) = hex_decoded_argument(&value) else { return Ok(value) };

    match options.get_uuid_representation() {
        Some(crate::prelude::UuidRepresentation::Blob) => {
            Ok(Expr::Cast {
                expr: Box::new(hex_argument),
                data_type: DataType::Uuid,
                format: None,
                kind: CastKind::DoubleColon,
            })
        }
        Some(crate::prelude::UuidRepresentation::Text) => {
            Err(Error::reverse_refusal(format!(
                "unhex({hex_argument}) cannot be reversed against a uuid column: this translation \
                 holds a uuid as text, so SQLite compares a blob with text and matches nothing, \
                 and no PostgreSQL expression answers that. Compare the column with the uuid's \
                 text instead."
            )))
        }
        None => {
            Err(Error::reverse_refusal(format!(
                "unhex({hex_argument}) cannot be reversed against a uuid column without knowing \
                 how this translation holds a uuid: the blob form reverses to a uuid cast and the \
                 text form has no faithful reversal at all. Set the uuid representation on the \
                 options."
            )))
        }
    }
}

/// The argument of a `decode(x, 'hex')` call, which is what the reverse
/// direction builds for SQLite's `unhex`.
fn hex_decoded_argument(value: &Expr) -> Option<Expr> {
    let Expr::Function(function) = value else { return None };
    if !function_is_named(function, "decode") {
        return None;
    }
    let FunctionArguments::List(list) = &function.args else { return None };
    let [first, second] = list.args.as_slice() else { return None };
    let second = second.to_string();
    if !second.eq_ignore_ascii_case("'hex'") {
        return None;
    }
    argument_expr(first).cloned()
}

/// `json_build_array(a, b)` -> `ARRAY[a, b]`, and the empty call -> an empty
/// array constructor cast to the column's type, which is the only form
/// PostgreSQL takes for one.
fn retype_json_array_as_array(value: Expr, declared: &DataType) -> Expr {
    let Expr::Function(function) = &value else { return value };
    if !function_is_named(function, "json_build_array")
        && !function_is_named(function, "jsonb_build_array")
    {
        return value;
    }
    let elements: Vec<Expr> = match &function.args {
        FunctionArguments::List(list) => {
            let Some(elements) =
                list.args.iter().map(|argument| argument_expr(argument).cloned()).collect()
            else {
                return value;
            };
            elements
        }
        FunctionArguments::None => Vec::new(),
        FunctionArguments::Subquery(_) => return value,
    };
    let constructor = Expr::Array(sqlparser::ast::Array { elem: elements, named: true });
    if matches!(&constructor, Expr::Array(array) if array.elem.is_empty()) {
        return Expr::Cast {
            expr: Box::new(constructor),
            data_type: declared.clone(),
            format: None,
            kind: CastKind::DoubleColon,
        };
    }
    constructor
}

/// The expression a function argument carries, ignoring the named forms no
/// storage wrapper uses.
fn argument_expr(argument: &sqlparser::ast::FunctionArg) -> Option<&Expr> {
    match argument {
        sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr))
        | sqlparser::ast::FunctionArg::Named {
            arg: sqlparser::ast::FunctionArgExpr::Expr(expr),
            ..
        }
        | sqlparser::ast::FunctionArg::ExprNamed {
            arg: sqlparser::ast::FunctionArgExpr::Expr(expr),
            ..
        } => Some(expr),
        _ => None,
    }
}

/// Whether `function` is the single-part name `name`, compared without case.
fn function_is_named(function: &sqlparser::ast::Function, name: &str) -> bool {
    let [ObjectNamePart::Identifier(ident)] = function.name.0.as_slice() else { return false };
    ident.value.eq_ignore_ascii_case(name)
}

/// True when `declared` is an array type, whose length PostgreSQL reads with
/// `array_length` rather than a JSON function.
pub(crate) fn is_array_type(declared: &DataType) -> bool {
    matches!(declared, DataType::Array(_))
}
