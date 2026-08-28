//! What a session variable mapping means, for everything that reads one.
//!
//! A mapping pairs a PostgreSQL setting with the function the replica registers
//! under. Going out, the pattern becomes the paired call and any cast over it
//! is dropped, because SQLite is dynamically typed and the replica's function
//! answers the value directly. Coming back, the paired call becomes the pattern
//! again and the cast is written from the type the mapping records.
//!
//! Single source of truth for both directions, and for the row-security
//! transformer, which used to carry its own copy of the substitution.

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

use sqlparser::ast::{
    CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, TimezoneInfo, Value, ValueWithSpan,
};

use crate::{
    errors::Error,
    impls::function_helpers::{simple_function_expr, single_quoted_literal, string_literal},
    options::Pg2SqliteOptions,
    traits::{SessionVariableMapping, SessionVariablePattern},
};

/// The lower-cased last part of a function's name.
#[must_use]
pub(crate) fn function_name_lower(name: &ObjectName) -> String {
    name.0.last().and_then(|part| part.as_ident()).map_or_else(
        || name.to_string().to_ascii_lowercase(),
        |ident| ident.value.to_ascii_lowercase(),
    )
}

/// The setting a `current_setting` call names, when it names one literally.
///
/// The second argument, PostgreSQL's `missing_ok`, is not read: both arities
/// answer the same setting, so both pair with the same function.
#[must_use]
fn setting_name(args: &FunctionArguments) -> Option<String> {
    let FunctionArguments::List(FunctionArgumentList { args, .. }) = args else {
        return None;
    };
    args.first().and_then(|arg| {
        match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))
            | FunctionArg::Named { arg: FunctionArgExpr::Expr(expr), .. } => {
                single_quoted_literal(expr).map(ToString::to_string)
            }
            _ => None,
        }
    })
}

/// The pattern a call matches, when it matches one a mapping can pair.
///
/// A `current_setting` whose name is computed rather than written yields
/// `None`: no mapping can be found for a setting only known at run time.
#[must_use]
pub(crate) fn pattern_of(name: &str, args: &FunctionArguments) -> Option<SessionVariablePattern> {
    match name {
        "current_user" => Some(SessionVariablePattern::CurrentUser),
        "current_setting" => {
            setting_name(args).map(|name| SessionVariablePattern::CurrentSetting { name })
        }
        _ => None,
    }
}

/// The pattern a function node matches.
#[must_use]
pub(crate) fn pattern_of_function(func: &Function) -> Option<SessionVariablePattern> {
    pattern_of(&function_name_lower(&func.name), &func.args)
}

/// The call the replica makes instead of the pattern.
#[must_use]
pub(crate) fn paired_call(mapping: &SessionVariableMapping) -> Expr {
    simple_function_expr(&mapping.sqlite_function, vec![], None)
}

/// The refusal a pattern with no mapping carries.
#[must_use]
pub(crate) fn unpaired(pattern: &SessionVariablePattern) -> Error {
    Error::SessionVariableMappingNotFound { pattern: pattern.to_string() }
}

/// The mapping whose paired function is `name`, when one is installed.
///
/// A single function can be reached from both patterns, which
/// [`Pg2SqliteOptions::with_session_user`] always arranges, and coming back
/// only one of them can be written. The setting wins: it names a value the
/// application binds per connection, where `current_user` names whatever role
/// the connection opened as, so the setting is the reading that carries the
/// caller. The same rule orders the two UUID arms in the reverse function
/// translator.
#[must_use]
pub(crate) fn mapping_for_function<'a>(
    name: &str,
    options: &'a Pg2SqliteOptions,
) -> Option<&'a SessionVariableMapping> {
    let paired =
        |mapping: &&SessionVariableMapping| mapping.sqlite_function.eq_ignore_ascii_case(name);
    let settings_first = |mapping: &&SessionVariableMapping| {
        matches!(mapping.pg_pattern, SessionVariablePattern::CurrentSetting { .. })
    };
    let mappings = options.get_session_variables();
    mappings
        .iter()
        .rev()
        .find(|mapping| paired(mapping) && settings_first(mapping))
        .or_else(|| mappings.iter().rev().find(paired))
}

/// The PostgreSQL expression the paired function stands for.
///
/// For a tolerant mapping (`missing_ok = true`, the default) the reverse emits
/// `current_setting(name, true)`, which answers NULL when the setting is unset.
/// For a strict mapping (`missing_ok = false`) the reverse emits
/// `current_setting(name)` with no second argument, which raises when the
/// setting is unset. The role keyword is written without an argument list,
/// since PostgreSQL refuses `current_user()` as a syntax error.
///
/// # Errors
///
/// Returns [`Error::SessionVariableTypeUnreadable`] when the recorded type does
/// not parse.
pub(crate) fn reverse_expression(mapping: &SessionVariableMapping) -> Result<Expr, Error> {
    let pattern = match &mapping.pg_pattern {
        SessionVariablePattern::CurrentSetting { name } => {
            if mapping.missing_ok {
                simple_function_expr(
                    "current_setting",
                    vec![
                        string_literal(name),
                        Expr::Value(ValueWithSpan {
                            value: Value::Boolean(true),
                            span: sqlparser::tokenizer::Span::empty(),
                        }),
                    ],
                    None,
                )
            } else {
                simple_function_expr("current_setting", vec![string_literal(name)], None)
            }
        }
        SessionVariablePattern::CurrentUser => {
            Expr::Function(Function {
                name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new("current_user"))]),
                uses_odbc_syntax: false,
                parameters: FunctionArguments::None,
                args: FunctionArguments::None,
                filter: None,
                null_treatment: None,
                over: None,
                within_group: vec![],
            })
        }
    };

    Ok(match mapping.pg_type_node()? {
        Some(data_type) => {
            Expr::Cast {
                expr: Box::new(pattern),
                data_type,
                format: None,
                kind: CastKind::DoubleColon,
            }
        }
        None => pattern,
    })
}

/// The refusal a paired function called with arguments carries.
///
/// The pairing states that one SQLite function answers one setting, and that
/// function takes nothing. A call with arguments is a different function
/// wearing the same name, and passing it through would emit a name PostgreSQL
/// does not have.
#[must_use]
pub(crate) fn paired_arity_refusal(mapping: &SessionVariableMapping) -> String {
    format!(
        "{}() pairs with {}, which takes no argument, so a call to it with arguments is not the \
         caller's identity and has no PostgreSQL equivalent.",
        mapping.sqlite_function, mapping.pg_pattern
    )
}

/// True when a call passes nothing, which is the only shape a paired function
/// has.
#[must_use]
pub(crate) fn call_has_no_arguments(args: &FunctionArguments) -> bool {
    match args {
        FunctionArguments::None => true,
        FunctionArguments::List(list) => list.args.is_empty(),
        FunctionArguments::Subquery(_) => false,
    }
}

/// What a cast over a session variable becomes, or `None` when the inner
/// expression is not a paired pattern and the ordinary cast path applies.
///
/// # Errors
///
/// Returns [`Error::SessionVariableTypeDisagrees`] when the written cast names
/// a different type than the mapping records, since the cast is dropped here
/// and written again from the recorded type on the way back.
pub(crate) fn translate_cast(
    inner: &Expr,
    data_type: &DataType,
    options: &Pg2SqliteOptions,
) -> Result<Option<Expr>, Error> {
    let Expr::Function(func) = inner else {
        return Ok(None);
    };
    let Some(pattern) = pattern_of_function(func) else {
        return Ok(None);
    };
    let Some(mapping) = options.find_session_variable(&pattern) else {
        return Ok(None);
    };

    if let Some(recorded) = mapping.pg_type_node()?
        && !same_postgres_type(&recorded, data_type)
    {
        return Err(Error::SessionVariableTypeDisagrees {
            pattern: pattern.to_string(),
            recorded: mapping.pg_type.clone().unwrap_or_default(),
            written: data_type.to_string(),
        });
    }

    Ok(Some(paired_call(mapping)))
}

/// Whether two spellings name one PostgreSQL type.
///
/// PostgreSQL's own aliases reach the parser as distinct variants, so `int` and
/// `integer` compare unequal as nodes while naming the same type. Folding them
/// is what lets a document keep its own spelling while a mapping records
/// another.
#[must_use]
pub(crate) fn same_postgres_type(left: &DataType, right: &DataType) -> bool {
    canonical(left) == canonical(right)
}

/// One representative per set of PostgreSQL aliases, parameters kept.
fn canonical(data_type: &DataType) -> DataType {
    match data_type {
        DataType::Int(width) | DataType::Integer(width) | DataType::Int4(width) => {
            DataType::Integer(*width)
        }
        DataType::SmallInt(width) | DataType::Int2(width) => DataType::SmallInt(*width),
        DataType::BigInt(width) | DataType::Int8(width) => DataType::BigInt(*width),
        DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => {
            DataType::Numeric(*info)
        }
        DataType::Real | DataType::Float4 => DataType::Real,
        DataType::DoublePrecision | DataType::Float8 => DataType::DoublePrecision,
        DataType::Varchar(length) | DataType::CharacterVarying(length) => {
            DataType::Varchar(*length)
        }
        DataType::Char(length) | DataType::Character(length) => DataType::Char(*length),
        DataType::Bool | DataType::Boolean => DataType::Boolean,
        DataType::VarBit(length) | DataType::BitVarying(length) => DataType::BitVarying(*length),
        DataType::Timestamp(precision, TimezoneInfo::Tz) => {
            DataType::Timestamp(*precision, TimezoneInfo::WithTimeZone)
        }
        DataType::Time(precision, TimezoneInfo::Tz) => {
            DataType::Time(*precision, TimezoneInfo::WithTimeZone)
        }
        DataType::Custom(name, modifiers) => {
            DataType::Custom(
                ObjectName(
                    name.0
                        .iter()
                        .map(|part| {
                            part.as_ident().map_or_else(
                                || part.clone(),
                                |ident| {
                                    sqlparser::ast::ObjectNamePart::Identifier(
                                        sqlparser::ast::Ident {
                                            value: ident.value.to_ascii_lowercase(),
                                            ..ident.clone()
                                        },
                                    )
                                },
                            )
                        })
                        .collect(),
                ),
                modifiers.clone(),
            )
        }
        other => other.clone(),
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    fn parse_type(spelling: &str) -> DataType {
        sqlparser::parser::Parser::new(&sqlparser::dialect::PostgreSqlDialect {})
            .try_with_sql(spelling)
            .expect("tokenises")
            .parse_data_type()
            .expect("parses as a type")
    }

    #[test]
    fn postgres_aliases_name_one_type() {
        for (left, right) in [
            ("int", "integer"),
            ("int4", "integer"),
            ("int2", "smallint"),
            ("int8", "bigint"),
            ("decimal(10,2)", "numeric(10,2)"),
            ("float4", "real"),
            ("float8", "double precision"),
            ("character varying(9)", "varchar(9)"),
            ("bool", "boolean"),
            ("timestamptz", "timestamp with time zone"),
            ("CITEXT", "citext"),
        ] {
            assert!(
                same_postgres_type(&parse_type(left), &parse_type(right)),
                "{left} and {right} are one PostgreSQL type"
            );
        }
    }

    #[test]
    fn different_types_stay_different() {
        for (left, right) in [
            ("int", "bigint"),
            ("uuid", "text"),
            ("numeric(10,2)", "numeric(10,3)"),
            ("varchar(9)", "varchar(10)"),
            ("timestamp", "timestamptz"),
            ("citext", "text"),
        ] {
            assert!(
                !same_postgres_type(&parse_type(left), &parse_type(right)),
                "{left} and {right} are different PostgreSQL types"
            );
        }
    }

    #[test]
    fn a_computed_setting_name_matches_no_pattern() {
        let args = FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(
                sqlparser::ast::Ident::new("owner"),
            )))],
            clauses: vec![],
        });
        assert!(pattern_of("current_setting", &args).is_none());
    }
}
