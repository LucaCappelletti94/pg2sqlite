//! Identifier handling for reverse-translated SQL.
//!
//! Three concerns live here:
//!
//! 1. **Quote normalisation.** SQLite accepts three delimited-identifier styles
//!    (`` `ident` ``, `[ident]`, and `"ident"`); PostgreSQL accepts only
//!    `"ident"`. Reverse translation presents its output as PostgreSQL, so
//!    every identifier it emits must carry `Some('"')` (or `None` when
//!    unquoted).
//!
//!    Only the quote style changes. The identifier text is preserved byte for
//!    byte. SQLite treats quoted identifiers case-insensitively while
//!    PostgreSQL double quotes make them case-sensitive, so reconciling a
//!    mixed-case SQLite identifier with a lower-case PostgreSQL table cannot
//!    be done by quoting rules alone and is out of scope. Under the
//!    shared-schema premise, where the consumer renders identifiers from the
//!    same schema the PostgreSQL side owns, the text already matches.
//!
//! 2. **Pseudo-expression quoting.** PostgreSQL evaluates certain bare
//!    lowercase names (`user`, `current_date`, …) as expressions rather than
//!    column references. When a replica column carries one of these names the
//!    reverse-translated SELECT must quote it so the server reads a column.
//!
//! 3. **SQLite-specific name refusal.** `main` and `temp` are SQLite database
//!    qualifiers with no PostgreSQL equivalent. `sqlite_master` and
//!    `sqlite_schema` are SQLite system tables. All of these are refused before
//!    the statement reaches the server.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{convert::Infallible, ops::ControlFlow};

use sqlparser::ast::{DoUpdate, Expr, Ident, ObjectName, Query, VisitMut, VisitorMut};

use crate::errors::Error;

// ─── 1. Quote normalisation
// ───────────────────────────────────────────────────

/// Rewrites backtick- and bracket-quoted identifiers to double-quoted, the only
/// delimited form PostgreSQL accepts.
struct IdentQuoteNormalizer;

impl VisitorMut for IdentQuoteNormalizer {
    type Break = Infallible;

    fn post_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<Self::Break> {
        if matches!(ident.quote_style, Some('`' | '[')) {
            ident.quote_style = Some('"');
        }
        ControlFlow::Continue(())
    }
}

/// Rewrites every backtick- or bracket-quoted identifier in `node` to
/// double-quoted, leaving unquoted (`None`) and already double-quoted
/// identifiers untouched and preserving identifier text byte for byte.
pub(crate) fn normalize_identifier_quotes<N: VisitMut>(node: &mut N) {
    let _ = node.visit(&mut IdentQuoteNormalizer);
}

// ─── 2. Pseudo-expression quoting ────────────────────────────────────────────

/// Bare lowercase names that PostgreSQL evaluates as pseudo-expressions when
/// unquoted: they answer the current role, schema, date, or time rather than
/// reading a column. A replica column with one of these names must be
/// double-quoted in the reverse-translated output.
pub(crate) const POSTGRES_PSEUDO_EXPRESSIONS: &[&str] = &[
    "user",
    "session_user",
    "current_user",
    "current_schema",
    "current_date",
    "current_time",
    "current_timestamp",
    "localtime",
    "localtimestamp",
];

/// True when `name` matches a PostgreSQL pseudo-expression spelling,
/// checked case-insensitively because unquoted SQLite identifiers may be any
/// case.
pub(crate) fn is_postgres_pseudo_expression(name: &str) -> bool {
    POSTGRES_PSEUDO_EXPRESSIONS.iter().any(|p| name.eq_ignore_ascii_case(p))
}

// ─── 3. DO UPDATE column qualification ───────────────────────────────────────

/// Qualifies bare column references in the `SET` and `WHERE` of a `DO UPDATE`
/// clause with `table_name`.
///
/// In SQLite an unqualified name in an `ON CONFLICT DO UPDATE` block resolves
/// to the existing target row. PostgreSQL demands a qualification because the
/// clause runs in a join against the `excluded` pseudo-table, making bare
/// references ambiguous. Prepending the table name tells the server to read the
/// existing target row, matching what the replica answered.
///
/// Identifiers already carrying a qualifier (`excluded.col`, `t.col`) are
/// `CompoundIdentifier` nodes and are left untouched. The `DEFAULT` keyword,
/// which sqlparser represents as a bare identifier, is also skipped.
/// Identifiers inside subqueries have their own scope and are not touched.
pub(crate) fn qualify_do_update_column_refs(do_update: &mut DoUpdate, table_name: &str) {
    let mut v = DoUpdateQualifier { table_name, subquery_depth: 0 };
    for assignment in &mut do_update.assignments {
        assignment.value.visit(&mut v);
    }
    if let Some(selection) = &mut do_update.selection {
        selection.visit(&mut v);
    }
}

struct DoUpdateQualifier<'a> {
    table_name: &'a str,
    subquery_depth: usize,
}

impl VisitorMut for DoUpdateQualifier<'_> {
    type Break = Infallible;

    fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
        self.subquery_depth += 1;
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
        self.subquery_depth -= 1;
        ControlFlow::Continue(())
    }

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if self.subquery_depth > 0 {
            return ControlFlow::Continue(());
        }
        if let Expr::Identifier(ident) = expr
            && !ident.value.eq_ignore_ascii_case("default")
        {
            let taken = core::mem::replace(ident, Ident::new(""));
            *expr = Expr::CompoundIdentifier(vec![Ident::new(self.table_name), taken]);
        }
        ControlFlow::Continue(())
    }
}

// ─── 4. SQLite-specific name refusal ─────────────────────────────────────────

/// Refuses an object name that carries a SQLite-only database qualifier or
/// names a SQLite system catalog.
///
/// `main` and `temp` are SQLite's built-in database names; PostgreSQL does not
/// use them as schema names. `sqlite_master` and `sqlite_schema` are SQLite
/// internal system tables that have no PostgreSQL equivalent.
pub(crate) fn refuse_sqlite_specific_names(name: &ObjectName) -> Result<(), Error> {
    // sqlite_master / sqlite_schema: SQLite system tables, no PG equivalent.
    if let Some(last) = name.0.last().and_then(|part| part.as_ident())
        && last.quote_style.is_none()
        && matches!(last.value.to_ascii_lowercase().as_str(), "sqlite_master" | "sqlite_schema")
    {
        return Err(Error::reverse_refusal(format!(
            "'{name}' is a SQLite system catalog table that does not exist in PostgreSQL. Query \
             information_schema.tables or pg_catalog.pg_tables instead."
        )));
    }
    // main / temp: SQLite built-in database qualifiers, not schema names.
    if name.0.len() >= 2
        && let Some(db) = name.0.first().and_then(|part| part.as_ident())
        && db.quote_style.is_none()
        && matches!(db.value.to_ascii_lowercase().as_str(), "main" | "temp")
    {
        return Err(Error::reverse_refusal(format!(
            "'{name}': '{db}' is a SQLite built-in database qualifier with no PostgreSQL \
             equivalent. Use the unqualified table name instead."
        )));
    }
    Ok(())
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::{dialect::SQLiteDialect, parser::Parser};

    use super::normalize_identifier_quotes;

    fn norm(sql: &str) -> String {
        let mut stmts = Parser::parse_sql(&SQLiteDialect {}, sql).unwrap();
        normalize_identifier_quotes(&mut stmts[0]);
        stmts[0].to_string()
    }

    #[test]
    fn normalizes_table_and_column_idents() {
        assert_eq!(norm("SELECT `t`.`c` FROM `t`"), r#"SELECT "t"."c" FROM "t""#);
    }

    #[test]
    fn normalizes_function_name_ident() {
        assert_eq!(norm("SELECT `max`(`c`) FROM `t`"), r#"SELECT "max"("c") FROM "t""#);
    }
}
