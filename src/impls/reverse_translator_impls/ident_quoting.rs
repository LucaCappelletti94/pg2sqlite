//! Normalizes SQLite delimited-identifier quoting to PostgreSQL double quotes
//! across a reverse-translated node.
//!
//! SQLite accepts three delimited-identifier styles (`` `ident` ``, `[ident]`,
//! and `"ident"`), PostgreSQL accepts only `"ident"`. Reverse translation
//! presents its output as PostgreSQL, so every identifier it emits must carry
//! `Some('"')` (or `None` when unquoted), never `` Some('`') `` or `Some('[')`.
//!
//! Only the quote style changes. The identifier text is preserved byte for
//! byte. SQLite treats quoted identifiers case-insensitively while PostgreSQL
//! double quotes make them case-sensitive, so reconciling a mixed-case SQLite
//! identifier with a lower-case PostgreSQL table cannot be done by quoting
//! rules alone and is out of scope. Under the shared-schema premise (the
//! consumer renders identifiers from the same schema the PostgreSQL side owns)
//! the text already matches.

use core::{convert::Infallible, ops::ControlFlow};

use sqlparser::ast::{Ident, VisitMut, VisitorMut};

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
