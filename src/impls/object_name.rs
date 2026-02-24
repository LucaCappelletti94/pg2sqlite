//! Helpers for structured [`sqlparser::ast::ObjectName`] manipulation.

use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

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

    if is_simple {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Builds a quoted qualified reference such as `NEW."column"`.
#[must_use]
pub(crate) fn prefixed_quoted_identifier(prefix: &str, name: &str) -> String {
    format!("{prefix}.{}", quote_identifier(name))
}

/// Creates an identifier that keeps double-quote style when formatted.
#[must_use]
pub(crate) fn quoted_ident(name: &str) -> Ident {
    if quote_identifier(name) == name {
        Ident::new(name)
    } else {
        Ident::with_quote('"', name)
    }
}

#[cfg(test)]
mod tests {
    use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

    use super::{
        append_suffix, last_ident, prefixed_quoted_identifier, quote_identifier, quoted_ident,
        schema_and_table_for_lookup,
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
}
