//! Helpers for structured [`sqlparser::ast::ObjectName`] manipulation.

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
    traits::{DatabaseLike, TableLike},
    utils::identifier_resolution::identifiers_match,
};
use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

use crate::errors::Error;

/// Returns the last identifier segment of an object name.
pub(crate) fn last_ident(name: &ObjectName) -> Option<&Ident> {
    name.0.last().and_then(ObjectNamePart::as_ident)
}

/// The lower-case catalogue name `ident` can name, or `None` when delimiting
/// makes it a name of its own.
///
/// PostgreSQL lowercases an undelimited identifier and keeps a delimited one
/// exactly, and every name this crate knows is lower case, so `"Vector"` and
/// `"RANDOM"` name something else entirely. Measured on 17.3: `"random"()`
/// resolves to the built-in while `"NOW"()` does not exist.
#[must_use]
pub(crate) fn catalog_name(ident: &Ident) -> Option<String> {
    let lowered = ident.value.to_ascii_lowercase();
    if ident.quote_style.is_some() && ident.value != lowered {
        return None;
    }
    Some(lowered)
}

/// The catalogue name the last segment of `name` can name.
#[must_use]
pub(crate) fn last_catalog_name(name: &ObjectName) -> Option<String> {
    last_ident(name).and_then(catalog_name)
}

/// The PostgreSQL catalogue function name named by an unqualified call or an
/// explicit `pg_catalog` call.
#[must_use]
pub(crate) fn postgres_catalog_function_name(name: &ObjectName) -> Option<String> {
    let function = last_ident(name).and_then(catalog_name)?;
    match name.0.as_slice() {
        [_] => Some(function),
        [schema, _]
            if schema
                .as_ident()
                .and_then(catalog_name)
                .is_some_and(|schema| schema == "pg_catalog") =>
        {
            Some(function)
        }
        _ => None,
    }
}

/// Returns the last identifier's value if `name` ends in a bare identifier.
/// Otherwise falls back to the full `ObjectName`'s `Display` form. Useful when
/// synthesizing single-quoted SQL string literals from a table or column
/// reference (e.g. `SELECT CreateSpatialIndex('features', 'geom')`).
#[must_use]
pub(crate) fn last_ident_value_or_display(name: &ObjectName) -> String {
    last_ident(name).map_or_else(|| name.to_string(), |ident| ident.value.clone())
}

/// Appends a suffix to the last identifier in an object name.
pub(crate) fn append_suffix(name: &ObjectName, suffix: &str) -> ObjectName {
    let mut updated = name.clone();
    if let Some(ObjectNamePart::Identifier(ident)) = updated.0.last_mut() {
        ident.value.push_str(suffix);
    }
    updated
}

/// Returns an object name normalized for SQLite by keeping only the terminal
/// identifier part.
#[must_use]
pub(crate) fn sqlite_unqualified_object_name(name: &ObjectName) -> ObjectName {
    name.0.last().cloned().map_or_else(|| name.clone(), |last| ObjectName(vec![last]))
}

fn unsupported_schema_qualification(name: &ObjectName, reason: &str) -> Error {
    Error::UnsupportedSchemaQualification {
        object_name: name.to_string(),
        reason: reason.to_string(),
    }
}

struct SchemaQualifiedNameParts<'a> {
    explicit_schema: Option<&'a Ident>,
    object_part: ObjectNamePart,
}

fn parse_schema_qualified_name_parts(
    name: &ObjectName,
) -> Result<SchemaQualifiedNameParts<'_>, Error> {
    match name.0.as_slice() {
        [object] => {
            object.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(SchemaQualifiedNameParts { explicit_schema: None, object_part: object.clone() })
        }
        [schema, object] => {
            let schema_ident = schema.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "schema segment must be an identifier")
            })?;
            object.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(SchemaQualifiedNameParts {
                explicit_schema: Some(schema_ident),
                object_part: object.clone(),
            })
        }
        _ => {
            Err(unsupported_schema_qualification(
                name,
                "object names with more than two parts are not supported",
            ))
        }
    }
}

fn schema_resolves(schema: &ParserDB, schema_name: &Ident) -> bool {
    identifiers_match("public", false, &schema_name.value, schema_name.quote_style.is_some())
        || schema.resolve_schema_ident(schema_name).is_some()
}

fn ensure_schema_resolves(
    schema: &ParserDB,
    name: &ObjectName,
    explicit_schema: Option<&Ident>,
) -> Result<(), Error> {
    if let Some(schema_name) = explicit_schema
        && !schema_resolves(schema, schema_name)
    {
        return Err(unsupported_schema_qualification(
            name,
            &format!("schema '{schema_name}' does not resolve in the translation schema"),
        ));
    }
    Ok(())
}

/// Validates schema qualification for forward translation under the
/// "resolvable schema" policy.
pub(crate) fn validate_schema_qualified_object_name_for_sqlite(
    schema: &ParserDB,
    name: &ObjectName,
) -> Result<(), Error> {
    let parts = parse_schema_qualified_name_parts(name)?;
    ensure_schema_resolves(schema, name, parts.explicit_schema)
}

/// Normalizes an object name to SQLite output shape while enforcing that any
/// explicit schema is resolvable in `schema`.
pub(crate) fn normalize_schema_qualified_object_name_for_sqlite(
    schema: &ParserDB,
    name: &ObjectName,
) -> Result<ObjectName, Error> {
    let parts = parse_schema_qualified_name_parts(name)?;
    ensure_schema_resolves(schema, name, parts.explicit_schema)?;
    Ok(ObjectName(vec![parts.object_part]))
}

/// Resolves a table under the crate's fixed implicit `public` policy.
pub(crate) fn resolve_translation_table<'a>(
    schema: &'a ParserDB,
    name: &ObjectName,
) -> Result<Option<&'a <ParserDB as DatabaseLike>::Table>, Error> {
    validate_schema_qualified_object_name_for_sqlite(schema, name)?;
    Ok(schema.resolve_table_object_name(name)?)
}

/// Returns whether a resolved translation table has row level security.
pub(crate) fn translation_table_has_rls(
    schema: &ParserDB,
    name: &ObjectName,
) -> Result<bool, Error> {
    match resolve_translation_table(schema, name)? {
        Some(table) => Ok(table.has_row_level_security(schema)?),
        None => Ok(false),
    }
}

/// Quotes an SQL identifier with double quotes, escaping interior quotes.
#[must_use]
pub(crate) fn quote_identifier(name: &str) -> String {
    let is_simple = match name.chars().next() {
        Some(first) => {
            (first == '_' || first.is_ascii_alphabetic())
                && name.chars().skip(1).all(|c| c == '_' || c.is_ascii_alphanumeric())
        }
        None => false,
    };

    if is_simple { name.to_string() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

/// Creates an identifier that keeps double-quote style when formatted.
#[must_use]
pub(crate) fn quoted_ident(name: &str) -> Ident {
    if quote_identifier(name) == name { Ident::new(name) } else { Ident::with_quote('"', name) }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Ident, ObjectName, ObjectNamePart, ObjectNamePartFunction},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        append_suffix, last_ident, normalize_schema_qualified_object_name_for_sqlite,
        quote_identifier, quoted_ident, resolve_translation_table, sqlite_unqualified_object_name,
        validate_schema_qualified_object_name_for_sqlite,
    };

    fn name(parts: &[&str]) -> ObjectName {
        ObjectName(
            parts.iter().map(|p| ObjectNamePart::Identifier(Ident::new(*p))).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn last_ident_and_append_suffix_work_for_identifier_names() {
        let object = name(&["public", "users"]);
        assert_eq!(last_ident(&object).map(|i| i.value.as_str()), Some("users"));

        let suffixed = append_suffix(&object, "_rls");
        assert_eq!(suffixed.to_string(), "public.users_rls");
    }

    #[test]
    fn identifier_quoting_helpers_escape_and_prefix_names() {
        assert_eq!(quote_identifier("simple_name"), "simple_name");
        assert_eq!(quote_identifier("a b"), "\"a b\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");

        let ident = quoted_ident("spaced ident");
        assert_eq!(ident.to_string(), "\"spaced ident\"");
        assert_eq!(quoted_ident("simple_name").to_string(), "simple_name");
    }

    #[test]
    fn sqlite_name_normalization_keeps_only_terminal_segment() {
        assert_eq!(sqlite_unqualified_object_name(&name(&["users"])).to_string(), "users");
        assert_eq!(
            sqlite_unqualified_object_name(&name(&["public", "users"])).to_string(),
            "users"
        );
        assert_eq!(
            sqlite_unqualified_object_name(&name(&["catalog", "public", "users"])).to_string(),
            "users"
        );
    }

    #[test]
    fn implicit_public_helpers_reject_non_public_and_three_part_names() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT PRIMARY KEY);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();

        let err =
            resolve_translation_table(&schema, &name(&["my_custom_app", "users"])).unwrap_err();
        assert!(err.to_string().contains("does not resolve in the translation schema"));

        let err =
            resolve_translation_table(&schema, &name(&["catalog", "public", "users"])).unwrap_err();
        assert!(err.to_string().contains("object names with more than two parts"));
    }

    #[test]
    fn schema_qualified_helpers_allow_resolvable_non_public_schemas_and_unqualify_output() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                "
                CREATE SCHEMA my_custom_app;
                CREATE TABLE my_custom_app.users(id INT PRIMARY KEY);
                ",
            )
            .unwrap(),
            "test".to_string(),
        )
        .unwrap();

        let name = name(&["my_custom_app", "users"]);
        validate_schema_qualified_object_name_for_sqlite(&schema, &name).unwrap();
        assert_eq!(
            normalize_schema_qualified_object_name_for_sqlite(&schema, &name).unwrap().to_string(),
            "users"
        );
        assert!(resolve_translation_table(&schema, &name).unwrap().is_some());
    }

    #[test]
    fn schema_qualified_helpers_reject_unresolvable_non_public_schemas() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT PRIMARY KEY);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();

        let err = validate_schema_qualified_object_name_for_sqlite(
            &schema,
            &name(&["my_custom_app", "users"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not resolve in the translation schema"));

        let err = normalize_schema_qualified_object_name_for_sqlite(
            &schema,
            &name(&["my_custom_app", "users"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not resolve in the translation schema"));
    }

    #[test]
    fn translation_table_lookup_rejects_non_identifier_segments() {
        let schema = ParserDB::from_statements(Vec::new(), "test".to_string()).unwrap();
        let function_part = ObjectNamePart::Function(ObjectNamePartFunction {
            name: Ident::new("remote"),
            args: vec![],
        });

        let err = resolve_translation_table(&schema, &ObjectName(vec![function_part.clone()]))
            .unwrap_err();
        assert!(err.to_string().contains("table segment must be an identifier"));

        let err = resolve_translation_table(
            &schema,
            &ObjectName(vec![
                function_part.clone(),
                ObjectNamePart::Identifier(Ident::new("users")),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("schema segment must be an identifier"));

        let err = resolve_translation_table(
            &schema,
            &ObjectName(vec![ObjectNamePart::Identifier(Ident::new("public")), function_part]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("table segment must be an identifier"));
    }

    #[test]
    fn implicit_public_lookup_resolves_public_and_unqualified_tables() {
        let unqualified_schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT PRIMARY KEY);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        assert!(
            resolve_translation_table(&unqualified_schema, &name(&["users"])).unwrap().is_some()
        );
        assert!(
            resolve_translation_table(&unqualified_schema, &name(&["public", "users"]))
                .unwrap()
                .is_some()
        );

        let public_schema = ParserDB::from_statements(
            Parser::parse_sql(
                &PostgreSqlDialect {},
                "CREATE TABLE public.users(id INT PRIMARY KEY);",
            )
            .unwrap(),
            "test".to_string(),
        )
        .unwrap();
        assert!(resolve_translation_table(&public_schema, &name(&["users"])).unwrap().is_some());
        assert!(
            resolve_translation_table(&public_schema, &name(&["public", "users"]))
                .unwrap()
                .is_some()
        );
    }
}
