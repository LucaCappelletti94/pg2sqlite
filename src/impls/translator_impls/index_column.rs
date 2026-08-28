//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `IndexColumn` type.

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

use sqlparser::ast::IndexColumn;

crate::traits::translator::impl_contextual_translator!(IndexColumn => IndexColumn);
impl crate::traits::translator::TranslatorWithContext for IndexColumn {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // SQLite has no operator classes, so the clause cannot be emitted:
        // `CREATE INDEX i ON t (s text_pattern_ops)` is `near
        // "text_pattern_ops": syntax error`. Dropping it does not change which
        // rows the database accepts, even on a UNIQUE index, where an opclass
        // is the one thing that could: PostgreSQL's pattern classes compare
        // bitwise while the default compares by collation, and those disagree
        // only under a nondeterministic collation, which `Expr::Collate`
        // already refuses.
        if let Some(operator_class) = &self.operator_class {
            emit(crate::warnings::TranslationWarning::LossyDowngrade {
                construct: "index operator class".to_string(),
                from: format!("{} {operator_class}", self.column),
                to: self.column.to_string(),
                location: self.column.to_string(),
                reason: "SQLite has no operator classes, so the index serves fewer queries than \
             the PostgreSQL one, notably the pattern matches a text pattern class \
             exists for."
                    .to_string(),
            });
        }

        // Every field is listed, with no `..self.clone()`, so a field added
        // upstream fails to compile here instead of reaching SQLite unexamined.
        Ok(IndexColumn {
            column: self.column.translate_with_warnings(schema, options, emit)?,
            operator_class: None,
        })
    }
}
