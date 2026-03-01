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
pub(crate) fn schema_and_table_for_lookup(name: &ObjectName) -> (Option<&str>, Option<&str>) {
    match name.0.as_slice() {
        [single] => (None, single.as_ident().map(|ident| ident.value.as_str())),
        [schema, table] => {
            (
                schema.as_ident().map(|ident| ident.value.as_str()),
                table.as_ident().map(|ident| ident.value.as_str()),
            )
        }
        _ => (None, last_ident(name).map(|ident| ident.value.as_str())),
    }
}

fn unsupported_schema_qualification(name: &ObjectName, reason: &str) -> Error {
    Error::UnsupportedSchemaQualification {
        object_name: name.to_string(),
        reason: reason.to_string(),
    }
}

fn implicit_public_lookup_parts(
    name: &ObjectName,
) -> Result<(Option<&str>, Option<&str>, &str), Error> {
    match name.0.as_slice() {
        [table] => {
            let table = table.as_ident().map(|ident| ident.value.as_str()).ok_or_else(|| {
                unsupported_schema_qualification(
                    name,
                    "object name must contain identifier segments",
                )
            })?;
            Ok((None, Some("public"), table))
        }
        [schema, table] => {
            let schema_ident = schema.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "schema segment must be an identifier")
            })?;
            if !schema_ident.value.eq_ignore_ascii_case("public") {
                return Err(unsupported_schema_qualification(
                    name,
                    "schema must be omitted or set to public",
                ));
            }
            let table = table.as_ident().map(|ident| ident.value.as_str()).ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok((Some("public"), None, table))
        }
        _ => {
            Err(unsupported_schema_qualification(
                name,
                "object names with more than two parts are not supported",
            ))
        }
    }
}

fn object_name_from_schema_and_table_part(
    schema: Option<&str>,
    table_part: ObjectNamePart,
) -> ObjectName {
    match schema {
        Some(schema) => {
            ObjectName(vec![ObjectNamePart::Identifier(Ident::new(schema)), table_part])
        }
        None => ObjectName(vec![table_part]),
    }
}

/// Returns an unqualified object name under the crate's implicit-public policy.
pub(crate) fn normalize_implicit_public_object_name(
    name: &ObjectName,
) -> Result<ObjectName, Error> {
    match name.0.as_slice() {
        [table] => {
            let _ = table.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(ObjectName(vec![table.clone()]))
        }
        [schema, table] => {
            let schema_ident = schema.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "schema segment must be an identifier")
            })?;
            if !schema_ident.value.eq_ignore_ascii_case("public") {
                return Err(unsupported_schema_qualification(
                    name,
                    "schema must be omitted or set to public",
                ));
            }
            let _ = table.as_ident().ok_or_else(|| {
                unsupported_schema_qualification(name, "table segment must be an identifier")
            })?;
            Ok(ObjectName(vec![table.clone()]))
        }
        _ => {
            Err(unsupported_schema_qualification(
                name,
                "object names with more than two parts are not supported",
            ))
        }
    }
}

/// Returns a canonical object name for schema-sensitive operations by choosing
/// the first schema variant that exists in `schema`.
pub(crate) fn normalize_implicit_public_object_name_for_schema(
    schema: &ParserDB,
    name: &ObjectName,
) -> Result<ObjectName, Error> {
    let (primary_schema, fallback_schema, table_name) = implicit_public_lookup_parts(name)?;
    let table_part = match name.0.as_slice() {
        [table] | [_, table] => table.clone(),
        _ => {
            return Err(unsupported_schema_qualification(
                name,
                "object names with more than two parts are not supported",
            ));
        }
    };

    if schema.table(primary_schema, table_name).is_some() {
        return Ok(object_name_from_schema_and_table_part(primary_schema, table_part.clone()));
    }
    if schema.table(fallback_schema, table_name).is_some() {
        return Ok(object_name_from_schema_and_table_part(fallback_schema, table_part.clone()));
    }

    Ok(object_name_from_schema_and_table_part(primary_schema, table_part))
}

/// Looks up a table under the crate's implicit-public policy.
///
/// For `table`, lookup order is: `None`, then `"public"`.
/// For `public.table`, lookup order is: `"public"`, then `None`.
pub(crate) fn table_with_implicit_public_lookup<'a>(
    schema: &'a ParserDB,
    name: &ObjectName,
) -> Result<Option<&'a <ParserDB as DatabaseLike>::Table>, Error> {
    let (primary_schema, fallback_schema, table_name) = implicit_public_lookup_parts(name)?;

    if let Some(table) = schema.table(primary_schema, table_name) {
        return Ok(Some(table));
    }

    if let Some(table) = schema.table(fallback_schema, table_name) {
        return Ok(Some(table));
    }

    Ok(None)
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
        ast::{Ident, ObjectName, ObjectNamePart},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use super::{
        append_suffix, last_ident, normalize_implicit_public_object_name,
        normalize_implicit_public_object_name_for_schema, prefixed_quoted_identifier,
        quote_identifier, quoted_ident, schema_and_table_for_lookup,
        sqlite_unqualified_object_name, table_with_implicit_public_lookup,
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
        assert_eq!(schema_and_table_for_lookup(&three), (None, Some("users")));
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
    fn implicit_public_lookup_accepts_unqualified_and_public_names() {
        assert_eq!(
            normalize_implicit_public_object_name(&name(&["users"])).unwrap().to_string(),
            "users"
        );
        assert_eq!(
            normalize_implicit_public_object_name(&name(&["public", "users"])).unwrap().to_string(),
            "users"
        );
    }

    #[test]
    fn implicit_public_lookup_rejects_non_public_and_three_part_names() {
        let err = normalize_implicit_public_object_name(&name(&["app", "users"])).unwrap_err();
        assert!(
            err.to_string()
                .contains("Only unqualified names and public.<table> names are supported")
        );

        let err = normalize_implicit_public_object_name(&name(&["catalog", "public", "users"]))
            .unwrap_err();
        assert!(err.to_string().contains("object names with more than two parts"));
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
        assert_eq!(
            normalize_implicit_public_object_name_for_schema(&public_schema, &name(&["users"]))
                .unwrap()
                .to_string(),
            "public.users"
        );
    }
}
