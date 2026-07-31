//! Translation of PostgreSQL arrays onto the SQLite `json1` extension.
//!
//! SQLite has no array type. Under
//! [`crate::traits::ArrayRepresentation::Json`] an array column becomes a
//! `TEXT` column holding a JSON array and every array operation is rewritten
//! against `json_array`, `json_array_length`, `json_insert`, `json_each`, and
//! `json_group_array`. With no representation configured every array construct
//! is rejected instead, so a schema that silently loses its arrays cannot be
//! emitted by accident.
//!
//! # Limits of the mapping
//!
//! * Only one-dimensional arrays of scalars round-trip. `json_each` returns a
//!   nested array or object as JSON text, which `json_group_array` re-encodes
//!   as a plain string, so `array_ndims`, `array_dims`, and slice subscripts
//!   stay errors.
//! * The forms that rebuild an array (`array_to_string`, `array_positions`,
//!   `array_remove`, `array_replace`) need SQLite 3.44 or newer for `ORDER BY`
//!   inside an aggregate.
//! * `unnest` translates only over a self-contained array. PostgreSQL reads
//!   `FROM t, unnest(t.col)` as an implicit LATERAL, which SQLite has no
//!   equivalent for.
//! * `x <op> ALL(arr)` and `a && b` yield false where PostgreSQL yields NULL
//!   for a NULL operand, or for an array containing NULL in the `ALL` case.
//!   Both exclude the row from a `WHERE` clause, which is the only place the
//!   difference is observable.

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

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArgumentClause, FunctionArguments, Ident, OrderByExpr,
    OrderByOptions, SelectItem, SetExpr, SetOperator, SetQuantifier, TableAlias, TableFactor,
    UnaryOperator, Value, ValueWithSpan, visit_expressions,
};

use crate::{
    errors::Error,
    impls::{
        expr_helpers::{case_when, null_safe_eq, null_safe_neq},
        function_helpers::{
            extract_exactly, integer_literal, simple_function_expr, string_literal,
        },
        query_builder::{
            from_relation, make_query, make_simple_select, single_expr_query, table_function_factor,
        },
    },
    prelude::{Pg2SqliteOptions, Translator},
    traits::{ArrayRepresentation, TranslationOptions},
};

/// Column of `json_each` holding the element value.
const VALUE_COLUMN: &str = "value";
/// Column of `json_each` holding the zero-based element index.
const KEY_COLUMN: &str = "key";

/// Which half of a concatenation a row came from, so the two orderings do not
/// interleave once both `key` sequences restart at zero.
const SIDE_COLUMN: &str = "pg2sqlite_side";

/// Name for the derived table holding both halves. SQLite needs no alias here,
/// but sqlparser renders one and a name nobody can collide with is cheaper than
/// finding out.
const HALVES_ALIAS: &str = "pg2sqlite_halves";

/// True when the caller opted into the JSON array representation.
#[must_use]
pub(crate) fn is_json_array_representation(options: &Pg2SqliteOptions) -> bool {
    matches!(options.get_array_representation(), Some(ArrayRepresentation::Json))
}

/// Message explaining that an array construct needs a storage representation
/// before it can be translated.
#[must_use]
pub(crate) fn representation_required_message(construct: &str) -> String {
    format!(
        "{construct} needs an array representation: SQLite has no array type, so pg2sqlite only \
         emits array translations when the caller declares how arrays are stored. Call \
         `Pg2SqliteOptions::with_array_representation(ArrayRepresentation::Json)` to store arrays \
         as JSON array text and translate array operations through the json1 extension."
    )
}

/// [`representation_required_message`] as an error.
#[must_use]
pub(crate) fn representation_required(construct: &str) -> Error {
    Error::UnsupportedSQLiteFeature(representation_required_message(construct))
}

/// Message for an array construct with no faithful `json1` form, even under
/// [`ArrayRepresentation::Json`].
#[must_use]
pub(crate) fn no_json_message(construct: &str, hint: &str) -> String {
    format!("{construct} has no equivalent over SQLite's JSON arrays. {hint}")
}

/// [`no_json_message`] as an error.
#[must_use]
pub(crate) fn no_json_equivalent(construct: &str, hint: &str) -> Error {
    Error::UnsupportedSQLiteFeature(no_json_message(construct, hint))
}

/// `json_each(<array>)` as a `FROM` item.
#[must_use]
fn json_each_factor(array: Expr) -> TableFactor {
    table_function_factor("json_each", vec![array], None, false)
}

/// A bare `value` / `key` reference to the current `json_each` row.
#[must_use]
fn json_each_column(name: &str) -> Expr {
    Expr::Identifier(Ident::new(name))
}

/// `(SELECT <projection> FROM json_each(<array>) [WHERE <selection>])`.
#[must_use]
fn scalar_subquery_over_json_each(projection: Expr, array: Expr, selection: Option<Expr>) -> Expr {
    Expr::Subquery(Box::new(single_expr_query(
        projection,
        from_relation(json_each_factor(array)),
        selection,
    )))
}

/// `a && b`, PostgreSQL's array overlap, as
/// `EXISTS (SELECT 1 FROM json_each(a) WHERE value IN (SELECT value FROM
/// json_each(b)))`.
///
/// The inner `value` resolves to the inner `json_each` and the outer one to the
/// outer, so the two sides really are compared, verified by executing a
/// disjoint pair and getting false. An empty array yields no rows, so `EXISTS`
/// is false, which matches PostgreSQL.
///
/// Like `x <op> ALL(arr)` in this module, a NULL operand yields false where
/// PostgreSQL yields NULL. See the divergence list in the module header.
pub(crate) fn array_overlap(left: Expr, right: Expr) -> Expr {
    let shares_an_element = Expr::InSubquery {
        expr: Box::new(json_each_column(VALUE_COLUMN)),
        subquery: Box::new(single_expr_query(
            json_each_column(VALUE_COLUMN),
            from_relation(json_each_factor(right)),
            None,
        )),
        negated: false,
    };

    Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(json_each_factor(left)),
            Some(shares_an_element),
        )),
        negated: false,
    }
}

/// `a || b` over arrays, as
/// `(SELECT json_group_array(value ORDER BY side, key) FROM (SELECT 0 AS side,
/// key, value FROM json_each(a) UNION ALL SELECT 1 AS side, key, value FROM
/// json_each(b)))`.
///
/// `side` is what keeps the two halves in order once their `key` sequences both
/// restart at zero, and ordering inside the aggregate rather than in the
/// subquery is what makes that order binding.
///
/// The same shape serves all three PostgreSQL spellings, since an operand that
/// is not an array is wrapped in `json_array` by the caller and `json_each` of
/// a one element array yields that one element.
///
/// A NULL operand expands to no rows, which is PostgreSQL's answer too: it
/// treats a NULL array as empty here rather than propagating. Two NULLs would
/// give an empty array where PostgreSQL gives NULL, so the caller guards that.
pub(crate) fn array_concat(left: Expr, right: Expr) -> Expr {
    let half = |side: i64, array: Expr| {
        Box::new(SetExpr::Select(Box::new(make_simple_select(
            vec![
                aliased(integer_literal(side), SIDE_COLUMN),
                SelectItem::UnnamedExpr(json_each_column(KEY_COLUMN)),
                SelectItem::UnnamedExpr(json_each_column(VALUE_COLUMN)),
            ],
            from_relation(json_each_factor(array)),
            None,
        ))))
    };

    let union = make_query(
        None,
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left: half(0, left),
            right: half(1, right),
        },
    );

    Expr::Subquery(Box::new(single_expr_query(
        ordered_aggregate_by(
            "json_group_array",
            vec![json_each_column(VALUE_COLUMN)],
            vec![SIDE_COLUMN, KEY_COLUMN],
        ),
        from_relation(TableFactor::Derived {
            lateral: false,
            subquery: Box::new(union),
            alias: Some(TableAlias {
                explicit: true,
                name: Ident::new(HALVES_ALIAS),
                columns: Vec::new(),
                at: None,
            }),
            sample: None,
        }),
        None,
    )))
}

/// Attach `ORDER BY key` to an aggregate call so the rebuilt array preserves
/// element order instead of relying on whatever scan order `json_each` happens
/// to produce.
#[must_use]
fn ordered_aggregate(name: &str, args: Vec<Expr>) -> Expr {
    ordered_aggregate_by(name, args, vec![KEY_COLUMN])
}

/// Attach `ORDER BY <columns>` to an aggregate call.
#[must_use]
fn ordered_aggregate_by(name: &str, args: Vec<Expr>, columns: Vec<&str>) -> Expr {
    let Expr::Function(mut func) = simple_function_expr(name, args, None) else {
        unreachable!("simple_function_expr always builds Expr::Function")
    };
    if let FunctionArguments::List(list) = &mut func.args {
        list.clauses.push(FunctionArgumentClause::OrderBy(
            columns
                .into_iter()
                .map(|column| {
                    OrderByExpr {
                        expr: json_each_column(column),
                        options: OrderByOptions::default(),
                        with_fill: None,
                    }
                })
                .collect(),
        ));
    }
    Expr::Function(func)
}

/// `json_array(<elements>)`, the translation of an `ARRAY[...]` literal.
#[must_use]
pub(crate) fn json_array_call(elements: Vec<Expr>) -> Expr {
    simple_function_expr("json_array", elements, None)
}

/// `json_array_length(<array>)`.
#[must_use]
fn json_array_length_call(array: Expr) -> Expr {
    simple_function_expr("json_array_length", vec![array], None)
}

/// Translate an `ARRAY[...]` (or bare `[...]`) literal.
pub(crate) fn translate_array_literal(
    elements: &[Expr],
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    if !is_json_array_representation(options) {
        return Err(representation_required("An ARRAY[...] literal"));
    }
    let translated =
        elements.iter().map(|e| e.translate(schema, options)).collect::<Result<Vec<_>, Error>>()?;
    Ok(json_array_call(translated))
}

/// Translate a one-based array subscript `arr[index]` into a `json_extract`
/// against the equivalent zero-based JSON path.
///
/// A literal index folds into a constant path. Any other index expression is
/// concatenated into the path at runtime and guarded so a subscript below one
/// yields NULL, as PostgreSQL does, instead of tripping SQLite's malformed JSON
/// path error. The guard reads the index expression a second time, which is
/// only observable for a volatile subscript such as `tags[1 + random()]`.
pub(crate) fn translate_array_subscript(
    root: Expr,
    index: &Expr,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    if !is_json_array_representation(options) {
        return Err(representation_required("Array subscripting"));
    }

    if let Some(literal) = integer_literal_value(index) {
        // PostgreSQL reads a subscript below the array's lower bound as a miss.
        if literal < 1 {
            return Ok(Expr::Value(ValueWithSpan::from(Value::Null)));
        }
        return Ok(json_extract_call(root, string_literal(&format!("$[{}]", literal - 1))));
    }

    let translated_index = index.translate(schema, options)?;
    let zero_based = Expr::Nested(Box::new(Expr::BinaryOp {
        left: Box::new(translated_index.clone()),
        op: BinaryOperator::Minus,
        right: Box::new(integer_literal(1)),
    }));
    let path = Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(string_literal("$[")),
            op: BinaryOperator::StringConcat,
            right: Box::new(zero_based),
        }),
        op: BinaryOperator::StringConcat,
        right: Box::new(string_literal("]")),
    };
    Ok(case_when(
        Expr::BinaryOp {
            left: Box::new(translated_index),
            op: BinaryOperator::GtEq,
            right: Box::new(integer_literal(1)),
        },
        json_extract_call(root, path),
        None,
    ))
}

/// `json_extract(<document>, <path>)`.
#[must_use]
fn json_extract_call(document: Expr, path: Expr) -> Expr {
    simple_function_expr("json_extract", vec![document, path], None)
}

/// Read an integer literal out of an expression so a constant subscript or
/// dimension argument can be folded at translation time.
#[must_use]
pub(crate) fn integer_literal_value(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Value(ValueWithSpan { value: Value::Number(text, _), .. }) => text.parse().ok(),
        Expr::Nested(inner) | Expr::UnaryOp { op: UnaryOperator::Plus, expr: inner } => {
            integer_literal_value(inner)
        }
        Expr::UnaryOp { op: UnaryOperator::Minus, expr: inner } => {
            integer_literal_value(inner).map(i64::wrapping_neg)
        }
        _ => None,
    }
}

/// A PostgreSQL array function whose body has to be rewritten rather than
/// renamed. `array_agg` and `cardinality` are absent because they map onto
/// `json_group_array` and `json_array_length` by name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArrayFunction {
    /// `array_length(a, dim)`.
    Length,
    /// `array_lower(a, dim)`.
    Lower,
    /// `array_upper(a, dim)`.
    Upper,
    /// `array_to_string(a, sep)`.
    ToString,
    /// `array_append(a, v)`.
    Append,
    /// `array_position(a, v)`.
    Position,
    /// `array_positions(a, v)`.
    Positions,
    /// `array_remove(a, v)`.
    Remove,
    /// `array_replace(a, from, to)`.
    Replace,
}

impl ArrayFunction {
    /// Match a lowercased PostgreSQL function name.
    #[must_use]
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "array_length" => Self::Length,
            "array_lower" => Self::Lower,
            "array_upper" => Self::Upper,
            "array_to_string" => Self::ToString,
            "array_append" => Self::Append,
            "array_position" => Self::Position,
            "array_positions" => Self::Positions,
            "array_remove" => Self::Remove,
            "array_replace" => Self::Replace,
            _ => return None,
        })
    }

    /// The PostgreSQL spelling, for error messages.
    #[must_use]
    const fn name(self) -> &'static str {
        match self {
            Self::Length => "array_length",
            Self::Lower => "array_lower",
            Self::Upper => "array_upper",
            Self::ToString => "array_to_string",
            Self::Append => "array_append",
            Self::Position => "array_position",
            Self::Positions => "array_positions",
            Self::Remove => "array_remove",
            Self::Replace => "array_replace",
        }
    }
}

/// Translate exactly `N` positional arguments of `kind`.
fn translated_args<const N: usize>(
    args: &FunctionArguments,
    kind: ArrayFunction,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<[Expr; N], Error> {
    let translated = extract_exactly(args, N, kind.name())?
        .into_iter()
        .map(|e| e.translate(schema, options))
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(translated.try_into().expect("extract_exactly guarantees the argument count"))
}

/// Translate a call to one of the [`ArrayFunction`] forms.
///
/// The dimension argument of `array_length` / `array_lower` / `array_upper`
/// must be the literal `1`: the JSON representation flattens to a single
/// dimension, so no other dimension has a bound to report.
pub(crate) fn translate_array_function(
    kind: ArrayFunction,
    args: &FunctionArguments,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    if !is_json_array_representation(options) {
        return Err(representation_required(&format!("{}()", kind.name())));
    }

    if kind == ArrayFunction::Replace {
        let [array, from, to] = translated_args(args, kind, schema, options)?;
        return Ok(array_replace(array, from, to));
    }

    let [array, second] = translated_args(args, kind, schema, options)?;
    match kind {
        ArrayFunction::Length | ArrayFunction::Lower | ArrayFunction::Upper => {
            if integer_literal_value(&second) != Some(1) {
                return Err(no_json_equivalent(
                    &format!("{}(a, {second})", kind.name()),
                    "A JSON array is one-dimensional, so only dimension 1 has a bound.",
                ));
            }
            Ok(if kind == ArrayFunction::Lower {
                array_lower_bound(array)
            } else {
                array_upper_bound(array)
            })
        }
        ArrayFunction::ToString => Ok(array_to_string(array, second)),
        ArrayFunction::Append => Ok(array_append(array, second)),
        ArrayFunction::Position => Ok(array_position(array, second)),
        ArrayFunction::Positions => Ok(array_positions(array, second)),
        ArrayFunction::Remove => Ok(array_remove(array, second)),
        ArrayFunction::Replace => unreachable!("handled above"),
    }
}

/// `array_length(a, 1)` and `array_upper(a, 1)`: the one-based upper bound,
/// which PostgreSQL reports as NULL for an empty array where
/// `json_array_length` reports zero.
#[must_use]
fn array_upper_bound(array: Expr) -> Expr {
    simple_function_expr("nullif", vec![json_array_length_call(array), integer_literal(0)], None)
}

/// `array_lower(a, 1)`: constant one for a non-empty array, NULL otherwise.
#[must_use]
fn array_lower_bound(array: Expr) -> Expr {
    case_when(
        Expr::BinaryOp {
            left: Box::new(json_array_length_call(array)),
            op: BinaryOperator::Gt,
            right: Box::new(integer_literal(0)),
        },
        integer_literal(1),
        None,
    )
}

/// `array_to_string(a, sep)`:
/// `(SELECT group_concat(value, sep ORDER BY key) FROM json_each(a))`.
#[must_use]
fn array_to_string(array: Expr, separator: Expr) -> Expr {
    scalar_subquery_over_json_each(
        ordered_aggregate("group_concat", vec![json_each_column(VALUE_COLUMN), separator]),
        array,
        None,
    )
}

/// `array_append(a, v)`: `json_insert(a, '$[#]', v)`, where `#` is SQLite's
/// one-past-the-end array index.
#[must_use]
fn array_append(array: Expr, value: Expr) -> Expr {
    simple_function_expr("json_insert", vec![array, string_literal("$[#]"), value], None)
}

/// `array_position(a, v)`:
/// `(SELECT min(key) + 1 FROM json_each(a) WHERE value IS v)`. A miss leaves
/// `min(key)` NULL, which propagates through the addition exactly as
/// PostgreSQL's NULL result.
///
/// The comparison is null safe because PostgreSQL finds a NULL element:
/// `array_position(ARRAY[1,NULL,3], NULL)` is 2, where `value = NULL` matches
/// nothing.
#[must_use]
fn array_position(array: Expr, value: Expr) -> Expr {
    scalar_subquery_over_json_each(
        Expr::BinaryOp {
            left: Box::new(simple_function_expr("min", vec![json_each_column(KEY_COLUMN)], None)),
            op: BinaryOperator::Plus,
            right: Box::new(integer_literal(1)),
        },
        array,
        Some(null_safe_eq(json_each_column(VALUE_COLUMN), value)),
    )
}

/// `array_positions(a, v)`:
/// `(SELECT json_group_array(key + 1 ORDER BY key) FROM json_each(a) WHERE
/// value IS v)`, an empty JSON array when nothing matches. Null safe for the
/// same reason as `array_position`.
#[must_use]
fn array_positions(array: Expr, value: Expr) -> Expr {
    scalar_subquery_over_json_each(
        ordered_aggregate(
            "json_group_array",
            vec![Expr::BinaryOp {
                left: Box::new(json_each_column(KEY_COLUMN)),
                op: BinaryOperator::Plus,
                right: Box::new(integer_literal(1)),
            }],
        ),
        array,
        Some(null_safe_eq(json_each_column(VALUE_COLUMN), value)),
    )
}

/// `array_remove(a, v)`:
/// `(SELECT json_group_array(value ORDER BY key) FROM json_each(a) WHERE NOT
/// (value IS v))`. The null-safe comparison keeps `array_remove(a, NULL)`
/// working, which a plain `<>` would not.
#[must_use]
fn array_remove(array: Expr, value: Expr) -> Expr {
    scalar_subquery_over_json_each(
        ordered_aggregate("json_group_array", vec![json_each_column(VALUE_COLUMN)]),
        array,
        Some(null_safe_neq(json_each_column(VALUE_COLUMN), value)),
    )
}

/// `array_replace(a, from, to)`:
/// `(SELECT json_group_array(CASE WHEN value IS from THEN to ELSE value END
/// ORDER BY key) FROM json_each(a))`.
#[must_use]
fn array_replace(array: Expr, from: Expr, to: Expr) -> Expr {
    let replaced = case_when(
        null_safe_eq(json_each_column(VALUE_COLUMN), from),
        to,
        Some(json_each_column(VALUE_COLUMN)),
    );
    scalar_subquery_over_json_each(
        ordered_aggregate("json_group_array", vec![replaced]),
        array,
        None,
    )
}

/// Whether a quantified comparison must hold for every element or just one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quantifier {
    /// `ANY` / `SOME`: true when at least one element satisfies the comparison.
    Any,
    /// `ALL`: true when no element fails the comparison.
    All,
}

/// Translate `left <op> ANY(array)` / `left <op> ALL(array)` where the operand
/// is an array value rather than a literal or a subquery.
///
/// `ANY` becomes `EXISTS (SELECT 1 FROM json_each(a) WHERE left <op> value)`.
/// `ALL` negates the same shape over the rows that fail, tested with
/// `IS NOT TRUE` so a NULL element makes the predicate false rather than true:
/// PostgreSQL yields NULL there, and false and NULL filter identically.
pub(crate) fn translate_quantified_over_array(
    translated_left: &Expr,
    compare_op: &BinaryOperator,
    array: Expr,
    quantifier: Quantifier,
    options: &Pg2SqliteOptions,
) -> Result<Expr, Error> {
    if !is_json_array_representation(options) {
        return Err(representation_required("A quantified comparison over an array value"));
    }

    let comparison = Expr::BinaryOp {
        left: Box::new(translated_left.clone()),
        op: compare_op.clone(),
        right: Box::new(json_each_column(VALUE_COLUMN)),
    };
    let (selection, negated) = match quantifier {
        Quantifier::Any => (comparison, false),
        Quantifier::All => (Expr::IsNotTrue(Box::new(Expr::Nested(Box::new(comparison)))), true),
    };

    Ok(Expr::Exists {
        subquery: Box::new(single_expr_query(
            integer_literal(1),
            from_relation(json_each_factor(array)),
            Some(selection),
        )),
        negated,
    })
}

/// PostgreSQL's default output column name for an unaliased `unnest(...)`.
const UNNEST_DEFAULT_COLUMN: &str = "unnest";
/// PostgreSQL's default ordinality column name for `WITH ORDINALITY`.
const ORDINALITY_DEFAULT_COLUMN: &str = "ordinality";

/// Translate `FROM unnest(<array>) [WITH ORDINALITY] [AS alias[(cols)]]` into a
/// derived table over `json_each`.
///
/// The derived table exists to name the element column. PostgreSQL names it
/// after the alias (or `unnest` when there is none), and SQLite accepts no
/// column list on a table alias, so the rename has to happen in a projection:
/// `unnest(tags) AS t` becomes `(SELECT value AS t FROM json_each(tags)) AS t`,
/// which keeps both `t` and `t.t` resolving as they do in PostgreSQL.
pub(crate) fn translate_unnest_factor(
    array_exprs: &[Expr],
    alias: Option<&TableAlias>,
    with_offset: bool,
    with_ordinality: bool,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<TableFactor, Error> {
    if !is_json_array_representation(options) {
        return Err(representation_required("UNNEST in a FROM clause"));
    }
    let [array_expr] = array_exprs else {
        return Err(no_json_equivalent(
            "UNNEST over several arrays",
            "Each array needs its own json_each() relation; join them on their key columns \
             instead.",
        ));
    };
    if with_offset {
        return Err(no_json_equivalent(
            "UNNEST ... WITH OFFSET",
            "WITH OFFSET is BigQuery syntax; use PostgreSQL's WITH ORDINALITY, which maps onto \
             json_each's key column.",
        ));
    }
    // PostgreSQL treats `FROM t, unnest(t.col)` as an implicit LATERAL. SQLite
    // has no LATERAL, and the derived table that supplies the output column
    // name cannot see a sibling FROM item, so the correlated form has no
    // translation that both runs and keeps the PostgreSQL column name.
    if references_a_column(array_expr) {
        return Err(no_json_equivalent(
            "UNNEST over a column reference",
            "PostgreSQL reads it as an implicit LATERAL, which SQLite has no equivalent for, and \
             SQLite accepts no column list on a table alias, so the element column cannot keep \
             its PostgreSQL name. Write `FROM t, json_each(t.col) AS e` and read `e.value`.",
        ));
    }

    let table_name = alias.map_or(UNNEST_DEFAULT_COLUMN, |a| a.name.value.as_str());
    let declared: Vec<&str> = alias
        .map(|a| a.columns.iter().map(|c| c.name.value.as_str()).collect())
        .unwrap_or_default();

    let element_name = declared.first().copied().unwrap_or(table_name);
    let mut projection = vec![aliased(json_each_column(VALUE_COLUMN), element_name)];
    if with_ordinality {
        let ordinal_name = declared.get(1).copied().unwrap_or(ORDINALITY_DEFAULT_COLUMN);
        projection.push(aliased(
            Expr::BinaryOp {
                left: Box::new(json_each_column(KEY_COLUMN)),
                op: BinaryOperator::Plus,
                right: Box::new(integer_literal(1)),
            },
            ordinal_name,
        ));
    }

    let translated_array = array_expr.translate(schema, options)?;
    Ok(TableFactor::Derived {
        lateral: false,
        subquery: Box::new(make_query(
            None,
            SetExpr::Select(Box::new(make_simple_select(
                projection,
                from_relation(json_each_factor(translated_array)),
                None,
            ))),
        )),
        alias: Some(TableAlias {
            explicit: true,
            name: Ident::new(table_name),
            columns: Vec::new(),
            at: None,
        }),
        sample: None,
    })
}

/// `<expr> AS <name>` as a projection item.
#[must_use]
fn aliased(expr: Expr, name: &str) -> SelectItem {
    SelectItem::ExprWithAlias { expr, alias: Ident::new(name) }
}

/// True when `expr` names a column anywhere inside it.
///
/// Walks every nested expression, including function arguments and subquery
/// bodies, so a correlated `unnest` operand cannot slip through as a
/// non-correlated one.
#[must_use]
fn references_a_column(expr: &Expr) -> bool {
    visit_expressions(expr, |inner| {
        if matches!(inner, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}
