//! Quoting for identifiers that SQLite reserves.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::{convert::Infallible, ops::ControlFlow};

use sqlparser::ast::{Expr, Ident, Statement, VisitMut, VisitorMut};

/// Keywords SQLite 3.51 rejects as a bare table, column or alias name, sorted
/// for binary search, without the `DEFAULT` the translator emits as a keyword.
const RESERVED: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "AUTOINCREMENT",
    "BETWEEN",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "DEFERRABLE",
    "DELETE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "FOREIGN",
    "FROM",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "LIMIT",
    "NOT",
    "NOTHING",
    "NOTNULL",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "PRIMARY",
    "RAISE",
    "RECURSIVE",
    "REFERENCES",
    "RETURNING",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
];

fn is_reserved(word: &str) -> bool {
    RESERVED
        .binary_search_by(|reserved| {
            reserved.bytes().cmp(word.bytes().map(|byte| byte.to_ascii_uppercase()))
        })
        .is_ok()
}

/// Quotes every bare identifier SQLite reserves, except function names, which
/// SQLite reads from the call syntax and which include the bare `RAISE` the
/// translator emits in trigger bodies.
#[derive(Default)]
struct ReservedIdentifierQuoter {
    function_names: Vec<*const Ident>,
}

impl VisitorMut for ReservedIdentifierQuoter {
    type Break = Infallible;

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(function) = expr {
            self.function_names.extend(
                function.name.0.iter().filter_map(|part| part.as_ident()).map(core::ptr::from_ref),
            );
        }
        ControlFlow::Continue(())
    }

    fn post_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<Self::Break> {
        if ident.quote_style.is_none()
            && is_reserved(&ident.value)
            && !self.function_names.contains(&core::ptr::from_ref(ident))
        {
            // PostgreSQL folds a bare identifier to lowercase, so the quoted
            // spelling is the folded one.
            ident.value.make_ascii_lowercase();
            ident.quote_style = Some('"');
        }
        ControlFlow::Continue(())
    }
}

/// Double-quotes every bare identifier in `statement` that SQLite reserves.
pub(crate) fn quote_reserved_identifiers(statement: &mut Statement) {
    let _ = statement.visit(&mut ReservedIdentifierQuoter::default());
}

#[cfg(test)]
mod tests {
    use super::{RESERVED, is_reserved};

    #[test]
    fn reserved_words_are_sorted_and_matched_without_case() {
        assert!(RESERVED.is_sorted());
        assert!(is_reserved("set") && is_reserved("Values") && is_reserved("NOT"));
        assert!(!is_reserved("default") && !is_reserved("key") && !is_reserved("replace"));
    }
}
