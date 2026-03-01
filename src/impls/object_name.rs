//! Helpers for structured [`sqlparser::ast::ObjectName`] manipulation.

use sql_traits::{
    structs::ParserDB,
    traits::{DatabaseLike, TableLike},
};
use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

use crate::errors::Error;

/// Returns the last identifier segment of an object name.
pub(crate) fn last_ident(name: &ObjectName) -> Option<&Ident> {
    name.0.last().and_then(ObjectNamePart::as_ident)
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

/// Returns the schema and table components used for schema lookup.
///
/// Supported forms:
/// - `table`
/// - `schema.table`
///
/// Any other shape returns `(None, None)`.
pub(crate) fn schema_and_table_for_lookup(name: &ObjectName) -> (Option<&str>, Option<&str>) {
    match name.0.as_slice() {
        [single] => (None, single.as_ident().map(|ident| ident.value.as_str())),
        [schema, table] => {
            (
                schema.as_ident().map(|ident| ident.value.as_str()),
                table.as_ident().map(|ident| ident.value.as_str()),
            )
        }
        _ => (None, None),
    }
}

fn unsupported_schema_qualification(name: &ObjectName, reason: &str) -> Error {
    Error::UnsupportedSchemaQualification {
        object_name: name.to_string(),
        reason: reason.to_string(),
    }
}

struct SchemaQualifiedNameParts<'a> {
    explicit_schema: Option<&'a str>,
    object_name: &'a str,
    object_part: ObjectNamePart,
}

fn parse_schema_qualified_name_parts(
    name: &ObjectName,
) -> Result<SchemaQualifiedNameParts<'_>, Error> {
    match name.0.as_slice() {
        [object] => {
            let object_ident = object.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(SchemaQualifiedNameParts {
                explicit_schema: None,
                object_name: object_ident.value.as_str(),
                object_part: object.clone(),
            })
        }
        [schema, object] => {
            let schema_ident = schema.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "schema segment must be an identifier")
            })?;
            let object_ident = object.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(SchemaQualifiedNameParts {
                explicit_schema: Some(schema_ident.value.as_str()),
                object_name: object_ident.value.as_str(),
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

fn schema_resolves(schema: &ParserDB, schema_name: &str) -> bool {
    if schema_name.eq_ignore_ascii_case("public") {
        return true;
    }

    if schema.schema(schema_name).is_some() {
        return true;
    }

    schema.tables().any(|table| {
        table
            .table_schema()
            .is_some_and(|table_schema| table_schema.eq_ignore_ascii_case(schema_name))
    })
}

fn ensure_schema_resolves(
    schema: &ParserDB,
    name: &ObjectName,
    explicit_schema: Option<&str>,
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

pub(crate) fn implicit_public_lookup_parts(
    name: &ObjectName,
) -> Result<(Option<&str>, Option<&str>, &str), Error> {
    let parts = parse_schema_qualified_name_parts(name)?;
    Ok(match parts.explicit_schema {
        None => (None, Some("public"), parts.object_name),
        Some(explicit_schema) if explicit_schema.eq_ignore_ascii_case("public") => {
            (Some("public"), None, parts.object_name)
        }
        Some(explicit_schema) => (Some(explicit_schema), None, parts.object_name),
    })
}

/// Looks up a table under forward-translation schema policy.
///
/// For `table`, lookup order is: `None`, then `"public"`.
/// For `public.table`, lookup order is: `"public"`, then `None`.
/// For `<schema>.table` where schema != `public`, lookup is exact schema only
/// and schema must resolve in `schema`.
pub(crate) fn table_with_implicit_public_lookup<'a>(
    schema: &'a ParserDB,
    name: &ObjectName,
) -> Result<Option<&'a <ParserDB as DatabaseLike>::Table>, Error> {
    let parts = parse_schema_qualified_name_parts(name)?;

    Ok(match parts.explicit_schema {
        None => {
            if let Some(table) = schema.table(None, parts.object_name) {
                Some(table)
            } else {
                schema.table(Some("public"), parts.object_name)
            }
        }
        Some(explicit_schema) if explicit_schema.eq_ignore_ascii_case("public") => {
            if let Some(table) = schema.table(Some("public"), parts.object_name) {
                Some(table)
            } else {
                schema.table(None, parts.object_name)
            }
        }
        Some(explicit_schema) => {
            ensure_schema_resolves(schema, name, Some(explicit_schema))?;
            schema.table(Some(explicit_schema), parts.object_name)
        }
    })
}

/// Returns whether an object name resolves to an RLS-protected table under the
/// crate's implicit-public policy.
pub(crate) fn table_has_implicit_public_rls(
    schema: &ParserDB,
    name: &ObjectName,
) -> Result<bool, Error> {
    Ok(table_with_implicit_public_lookup(schema, name)?
        .is_some_and(|table| table.has_row_level_security(schema)))
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

/// Builds a quoted qualified reference such as `NEW."column"`.
#[must_use]
pub(crate) fn prefixed_quoted_identifier(prefix: &str, name: &str) -> String {
    format!("{prefix}.{}", quote_identifier(name))
}

/// Creates an identifier that keeps double-quote style when formatted.
#[must_use]
pub(crate) fn quoted_ident(name: &str) -> Ident {
    if quote_identifier(name) == name { Ident::new(name) } else { Ident::with_quote('"', name) }
}

#[cfg(test)]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{Ident, ObjectName, ObjectNamePart, ObjectNamePartFunction},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        append_suffix, implicit_public_lookup_parts, last_ident,
        normalize_schema_qualified_object_name_for_sqlite, prefixed_quoted_identifier,
        quote_identifier, quoted_ident, schema_and_table_for_lookup,
        sqlite_unqualified_object_name, table_with_implicit_public_lookup,
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
    fn schema_and_table_lookup_supports_single_two_and_multi_part_names() {
        let single = name(&["users"]);
        assert_eq!(schema_and_table_for_lookup(&single), (None, Some("users")));

        let two = name(&["public", "users"]);
        assert_eq!(schema_and_table_for_lookup(&two), (Some("public"), Some("users")));

        let three = name(&["catalog", "public", "users"]);
        assert_eq!(schema_and_table_for_lookup(&three), (None, None));
    }

    #[test]
    fn identifier_quoting_helpers_escape_and_prefix_names() {
        assert_eq!(quote_identifier("simple_name"), "simple_name");
        assert_eq!(quote_identifier("a b"), "\"a b\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(prefixed_quoted_identifier("NEW", "a b"), "NEW.\"a b\"");

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
    fn implicit_public_lookup_parts_handle_unqualified_public_and_explicit_schema() {
        assert_eq!(
            implicit_public_lookup_parts(&name(&["users"])).unwrap(),
            (None, Some("public"), "users")
        );
        assert_eq!(
            implicit_public_lookup_parts(&name(&["public", "users"])).unwrap(),
            (Some("public"), None, "users")
        );
        assert_eq!(
            implicit_public_lookup_parts(&name(&["my_custom_app", "users"])).unwrap(),
            (Some("my_custom_app"), None, "users")
        );
    }

    #[test]
    fn implicit_public_lookup_parts_reject_three_part_names() {
        let err = implicit_public_lookup_parts(&name(&["catalog", "public", "users"])).unwrap_err();
        assert!(err.to_string().contains("object names with more than two parts"));
    }

    #[test]
    fn implicit_public_helpers_reject_non_public_and_three_part_names() {
        let schema = ParserDB::from_statements(
            Parser::parse_sql(&PostgreSqlDialect {}, "CREATE TABLE users(id INT PRIMARY KEY);")
                .unwrap(),
            "test".to_string(),
        )
        .unwrap();

        let err = table_with_implicit_public_lookup(&schema, &name(&["my_custom_app", "users"]))
            .unwrap_err();
        assert!(err.to_string().contains("does not resolve in the translation schema"));

        let err =
            table_with_implicit_public_lookup(&schema, &name(&["catalog", "public", "users"]))
                .unwrap_err();
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
        assert!(table_with_implicit_public_lookup(&schema, &name).unwrap().is_some());
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
    fn implicit_public_lookup_rejects_non_identifier_segments() {
        let function_part = ObjectNamePart::Function(ObjectNamePartFunction {
            name: Ident::new("remote"),
            args: vec![],
        });

        let err =
            implicit_public_lookup_parts(&ObjectName(vec![function_part.clone()])).unwrap_err();
        assert!(err.to_string().contains("table segment must be an identifier"));

        let err = implicit_public_lookup_parts(&ObjectName(vec![
            function_part.clone(),
            ObjectNamePart::Identifier(Ident::new("users")),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("schema segment must be an identifier"));

        let err = implicit_public_lookup_parts(&ObjectName(vec![
            ObjectNamePart::Identifier(Ident::new("public")),
            function_part,
        ]))
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
            table_with_implicit_public_lookup(&unqualified_schema, &name(&["users"]))
                .unwrap()
                .is_some()
        );
        assert!(
            table_with_implicit_public_lookup(&unqualified_schema, &name(&["public", "users"]))
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
        assert!(
            table_with_implicit_public_lookup(&public_schema, &name(&["users"])).unwrap().is_some()
        );
        assert!(
            table_with_implicit_public_lookup(&public_schema, &name(&["public", "users"]))
                .unwrap()
                .is_some()
        );
    }
}
