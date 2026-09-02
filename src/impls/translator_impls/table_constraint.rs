//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `TableConstraint` type.

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

use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    BinaryOperator, ConstraintReferenceMatchKind, Expr, Ident, IndexColumn, NullsDistinctOption,
    TableConstraint,
};

use crate::{
    impls::{
        object_name::{append_suffix, translation_table_has_rls},
        shared_helpers::{
            match_partial_not_supported_error, nulls_not_distinct_not_supported_error,
        },
        translator_impls::constraint_characteristic::deferrability_outside_a_foreign_key,
    },
    traits::translator::TranslatorWithContext,
};

crate::traits::translator::impl_contextual_translator!(
    TableConstraint => Vec<TableConstraint>
);
/// A constraint can translate to none, as a dropped CHECK does, to one, or
/// to several: a composite `MATCH FULL` foreign key needs a CHECK beside
/// it because SQLite ignores the MATCH clause.
impl crate::traits::translator::TranslatorWithContext for TableConstraint {
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match self {
            Self::Check(check_constraint) => {
                match check_constraint.expr.translate_with_warnings(schema, options, emit) {
                    Ok(translated_expr) => {
                        crate::impls::translator_impls::column_option::warn_no_inherit_dropped(
                            check_constraint,
                            emit,
                        );
                        Ok(vec![Self::Check(sqlparser::ast::CheckConstraint {
                            name: check_constraint.name.clone(),
                            expr: Box::new(translated_expr),
                            enforced: check_constraint.enforced,
                            no_inherit: false,
                        })])
                    }
                    Err(_) if options.is_remove_unsupported_check_constraints_enabled() => {
                        Ok(Vec::new())
                    }
                    Err(e) => Err(e),
                }
            }
            Self::ForeignKey(fk_constraint) => {
                let mut updated_fk = fk_constraint.clone();

                if translation_table_has_rls(schema, &fk_constraint.foreign_table)? {
                    updated_fk.foreign_table =
                        append_suffix(&fk_constraint.foreign_table, options.get_rls_table_suffix());
                }

                updated_fk.on_delete = fk_constraint
                    .on_delete
                    .map(|a| a.translate_with_warnings(schema, options, emit))
                    .transpose()?;
                updated_fk.on_update = fk_constraint
                    .on_update
                    .map(|a| a.translate_with_warnings(schema, options, emit))
                    .transpose()?;

                updated_fk.characteristics = fk_constraint
                    .characteristics
                    .map(|c| c.translate_with_warnings(schema, options, emit))
                    .transpose()?;

                let mut constraints = vec![Self::ForeignKey(updated_fk)];
                constraints.extend(match_full_guard(fk_constraint)?);
                Ok(constraints)
            }
            Self::PrimaryKey(pk_constraint) => {
                let mut updated_pk = pk_constraint.clone();
                updated_pk.columns =
                    translate_index_columns(&pk_constraint.columns, schema, options, emit)?;
                if let Some(characteristics) = pk_constraint.characteristics {
                    return Err(deferrability_outside_a_foreign_key(
                        "PRIMARY KEY",
                        characteristics,
                    ));
                }
                Ok(vec![Self::PrimaryKey(updated_pk)])
            }
            Self::Unique(unique_constraint) => {
                if matches!(unique_constraint.nulls_distinct, NullsDistinctOption::NotDistinct) {
                    return Err(nulls_not_distinct_not_supported_error());
                }

                let mut updated_unique = unique_constraint.clone();
                updated_unique.columns =
                    translate_index_columns(&unique_constraint.columns, schema, options, emit)?;
                if let Some(characteristics) = unique_constraint.characteristics {
                    return Err(deferrability_outside_a_foreign_key("UNIQUE", characteristics));
                }
                // `NULLS DISTINCT` is the default and is what SQLite does, so
                // the clause is dropped rather than emitted: SQLite rejects it
                // with `near "NULLS": syntax error`.
                updated_unique.nulls_distinct = NullsDistinctOption::None;
                Ok(vec![Self::Unique(updated_unique)])
            }
            // Outcome 2 of the reporting policy in `statement.rs`. The
            // passthrough that used to stand here emitted every one of these
            // verbatim, and none has a SQLite form, so each produced SQL that
            // could not run. There is no wildcard arm on purpose: a new
            // `sqlparser` variant fails to compile until it is classified.
            Self::Exclude(exclude) => {
                Err(crate::errors::Error::forward_refusal(format!(
                    "{exclude} cannot be translated. An exclusion constraint enforces that no two \
                         rows satisfy a comparison, which SQLite has no constraint for. Express it \
                         with a trigger that raises, or with a unique index where the comparison is \
                         equality."
                )))
            }
            Self::Index(index) => {
                Err(crate::errors::Error::forward_refusal(format!(
                    "{index} cannot be translated. SQLite declares an index with a CREATE INDEX \
                         statement of its own rather than inside the table body."
                )))
            }
            Self::FulltextOrSpatial(constraint) => {
                Err(crate::errors::Error::forward_refusal(format!(
                    "{constraint} cannot be translated. SQLite offers full-text search through the \
                         FTS5 virtual table rather than a key on an ordinary table."
                )))
            }
            Self::PrimaryKeyUsingIndex(constraint) | Self::UniqueUsingIndex(constraint) => {
                Err(crate::errors::Error::forward_refusal(format!(
                    "USING INDEX {} cannot be translated. It adopts an existing index as the \
                         constraint, and SQLite has no way to promote an index that way. Declare the \
                         constraint on the table instead.",
                    constraint.index_name
                )))
            }
        }
    }
}
fn translate_index_columns(
    columns: &[IndexColumn],
    schema: &ParserDB,
    options: &crate::options::TranslationContext<'_>,
    emit: crate::warnings::WarningSink<'_>,
) -> Result<Vec<IndexColumn>, crate::errors::Error> {
    columns.iter().map(|column| column.translate_with_warnings(schema, options, emit)).collect()
}

/// The CHECK that makes a composite `MATCH FULL` foreign key behave the way
/// PostgreSQL does, or `None` when the declaration needs none.
///
/// PostgreSQL's MATCH FULL says the key columns are either wholly NULL, which
/// exempts the row, or wholly non-NULL, which requires a match. SQLite parses
/// the MATCH clause and then always behaves as MATCH SIMPLE, where one NULL
/// anywhere exempts the row, so the mixture was accepted in silence. The
/// foreign key already gives both of PostgreSQL's cases; all that is missing
/// is refusing the mixture, which is what this CHECK does, and it covers
/// UPDATE as well as INSERT. Measured against PostgreSQL 17 and SQLite 3.46.0,
/// the pair accepts and refuses exactly the same rows.
///
/// One column needs nothing, since with a single column the two readings
/// coincide. `MATCH SIMPLE` and an absent clause are what SQLite already does.
fn match_full_guard(
    fk: &sqlparser::ast::ForeignKeyConstraint,
) -> Result<Option<TableConstraint>, crate::errors::Error> {
    match fk.match_kind {
        None | Some(ConstraintReferenceMatchKind::Simple) => return Ok(None),
        Some(ConstraintReferenceMatchKind::Partial) => {
            return Err(match_partial_not_supported_error());
        }
        Some(ConstraintReferenceMatchKind::Full) => {}
    }
    if fk.columns.len() < 2 {
        return Ok(None);
    }

    let all_null = fk
        .columns
        .iter()
        .map(|column| Expr::IsNull(Box::new(Expr::Identifier(column.clone()))))
        .reduce(and);
    let none_null = fk
        .columns
        .iter()
        .map(|column| Expr::IsNotNull(Box::new(Expr::Identifier(column.clone()))))
        .reduce(and);
    let (Some(all_null), Some(none_null)) = (all_null, none_null) else {
        return Ok(None);
    };

    Ok(Some(TableConstraint::Check(sqlparser::ast::CheckConstraint {
        // Named, because an anonymous CHECK reports its whole expression and
        // says nothing about the constraint it stands for.
        name: Some(Ident::new(guard_name(fk))),
        expr: Box::new(Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(all_null))),
            op: BinaryOperator::Or,
            right: Box::new(Expr::Nested(Box::new(none_null))),
        }),
        enforced: None,
        no_inherit: false,
    })))
}

/// `left AND right`.
fn and(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: Box::new(left), op: BinaryOperator::And, right: Box::new(right) }
}

/// The guard's constraint name: the foreign key's own name when it has one,
/// and its column list otherwise. A CHECK name is scoped to its table, so the
/// table name is not needed to keep it unique.
fn guard_name(fk: &sqlparser::ast::ForeignKeyConstraint) -> String {
    let stem = fk.name.as_ref().map_or_else(
        || fk.columns.iter().map(|column| column.value.as_str()).collect::<Vec<_>>().join("_"),
        |name| name.value.clone(),
    );
    format!("{stem}_match_full")
}
