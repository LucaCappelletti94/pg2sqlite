//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `Column` type.

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
    ColumnOption, ColumnOptionDef, ConstraintReferenceMatchKind, Expr, ForeignKeyConstraint,
    FunctionArguments, GeneratedAs, UnaryOperator, Value, ValueWithSpan,
};

use crate::impls::{
    object_name::{append_suffix, table_has_implicit_public_rls},
    shared_helpers::match_partial_not_supported_error,
    translator_impls::expr::sqlite_collation,
};

/// Warn that a `CHECK ... NO INHERIT` loses its modifier.
///
/// Provably neutral: `NO INHERIT` only stops a constraint from reaching child
/// tables, SQLite has no table inheritance, and `INHERITS` itself is refused,
/// so no child exists for the constraint to be withheld from.
pub(crate) fn warn_no_inherit_dropped(
    check: &sqlparser::ast::CheckConstraint,
    emit: crate::warnings::WarningSink<'_>,
) {
    if check.no_inherit {
        emit(crate::warnings::TranslationWarning::LossyDrop {
            construct: "CHECK ... NO INHERIT".to_string(),
            reason: "NO INHERIT only stops a constraint from reaching child tables, and \
                     SQLite has no table inheritance, so the modifier was dropped and the \
                     CHECK itself is kept."
                .to_string(),
        });
    }
}

crate::traits::translator::impl_contextual_translator!(
    ColumnOptionDef => Option<ColumnOptionDef>
);
impl crate::traits::translator::TranslatorWithContext for ColumnOptionDef {
    #[allow(clippy::too_many_lines)]
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        match &self.option {
            ColumnOption::Unique(unique_constraint) => {
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: unique_constraint.clone().into(),
                }))
            }
            ColumnOption::Default(expr) => {
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: ColumnOption::Default(parenthesize_default(
                        expr.translate_with_warnings(schema, options, emit)?,
                    )),
                }))
            }
            ColumnOption::NotNull | ColumnOption::PrimaryKey(_) => Ok(Some(self.clone())),
            // Translate CHECK constraints to SQLite CHECK syntax.
            // When `remove_unsupported_check_constraints` is set, silently drop them instead.
            ColumnOption::Check(check) => {
                if options.is_remove_unsupported_check_constraints_enabled() {
                    Ok(None)
                } else {
                    warn_no_inherit_dropped(check, emit);
                    let translated_expr =
                        check.expr.translate_with_warnings(schema, options, emit)?;
                    Ok(Some(ColumnOptionDef {
                        name: self.name.clone(),
                        option: ColumnOption::Check(sqlparser::ast::CheckConstraint {
                            name: check.name.clone(),
                            expr: Box::new(translated_expr),
                            enforced: check.enforced,
                            no_inherit: false,
                        }),
                    }))
                }
            }
            // Silently drop options that are either SQLite defaults or have no
            // SQLite equivalent and no runtime semantic effect.
            ColumnOption::Null | ColumnOption::CharacterSet(_) | ColumnOption::Comment(_) => {
                Ok(None)
            }
            // A collation changes every comparison, ordering, and unique check
            // over the column, so it is mapped or refused by the same rule the
            // expression path uses, never dropped.
            ColumnOption::Collation(collation) => {
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: ColumnOption::Collation(sqlite_collation(collation)?),
                }))
            }
            // Generated columns: GENERATED ALWAYS AS (expr) [STORED | VIRTUAL]
            // SQLite supports this syntax since version 3.31.0
            ColumnOption::Generated {
                generated_as,
                sequence_options: _,
                generation_expr,
                generation_expr_mode,
                generated_keyword,
            } => {
                if *generated_as == GeneratedAs::ByDefault {
                    return Err(crate::errors::Error::forward_refusal(
                        "SQLite only supports GENERATED ALWAYS, not GENERATED BY DEFAULT"
                            .to_string(),
                    ));
                }

                let translated_expr = generation_expr
                    .as_ref()
                    .map(|e| e.translate_with_warnings(schema, options, emit))
                    .transpose()?;

                // SQLite supports both STORED and VIRTUAL modes
                // VIRTUAL is the default in SQLite if not specified
                Ok(Some(ColumnOptionDef {
                    name: self.name.clone(),
                    option: ColumnOption::Generated {
                        generated_as: *generated_as,
                        sequence_options: None,
                        generation_expr: translated_expr,
                        generation_expr_mode: *generation_expr_mode,
                        generated_keyword: *generated_keyword,
                    },
                }))
            }
            ColumnOption::ForeignKey(ForeignKeyConstraint {
                name,
                index_name,
                columns,
                match_kind,
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                characteristics,
            }) => {
                // A column-level foreign key is single-column by
                // construction, so MATCH FULL reads the same as the default
                // MATCH SIMPLE and needs no guard. MATCH PARTIAL is refused
                // for the same reason the table-level spelling is.
                if matches!(match_kind, Some(ConstraintReferenceMatchKind::Partial)) {
                    return Err(match_partial_not_supported_error());
                }
                let updated_foreign_table = {
                    if table_has_implicit_public_rls(schema, foreign_table)? {
                        append_suffix(foreign_table, options.get_rls_table_suffix())
                    } else {
                        foreign_table.clone()
                    }
                };

                Ok(Some(Self {
                    name: self.name.clone(),
                    option: ForeignKeyConstraint {
                        name: name.clone(),
                        index_name: index_name.clone(),
                        columns: columns.clone(),
                        match_kind: *match_kind,
                        foreign_table: updated_foreign_table,
                        referred_columns: referred_columns.clone(),
                        on_delete: on_delete
                            .map(|on_delete| {
                                on_delete.translate_with_warnings(schema, options, emit)
                            })
                            .transpose()?,
                        on_update: on_update
                            .map(|on_update| {
                                on_update.translate_with_warnings(schema, options, emit)
                            })
                            .transpose()?,
                        characteristics: characteristics
                            .map(|c| c.translate_with_warnings(schema, options, emit))
                            .transpose()?,
                    }
                    .into(),
                }))
            }
            other => {
                Err(crate::errors::Error::forward_refusal(format!(
                    "Unsupported column option: {other}"
                )))
            }
        }
    }
}

/// Parenthesize a translated `DEFAULT` operand when SQLite would reject it
/// bare.
///
/// SQLite's `DEFAULT` clause takes a literal value, a signed number, a bare
/// keyword, or a parenthesized expression. A bare function call, cast, or
/// operator is a syntax error, so `DEFAULT json_array()` and
/// `DEFAULT CAST(x AS TEXT)` have to become `DEFAULT (json_array())` and
/// `DEFAULT (CAST(x AS TEXT))`.
fn parenthesize_default(expr: Expr) -> Expr {
    let accepted_bare = match &expr {
        // A literal, an already parenthesized expression, or a bare word.
        Expr::Value(_) | Expr::Nested(_) | Expr::Identifier(_) => true,
        // A signed number, but not a signed expression.
        Expr::UnaryOp { op: UnaryOperator::Plus | UnaryOperator::Minus, expr: inner } => {
            matches!(inner.as_ref(), Expr::Value(ValueWithSpan { value: Value::Number(..), .. }))
        }
        // Keyword functions such as `CURRENT_TIMESTAMP` carry no argument list
        // and render without parentheses, which SQLite accepts as a bare word.
        Expr::Function(func) => matches!(func.args, FunctionArguments::None),
        _ => false,
    };
    if accepted_bare { expr } else { Expr::Nested(Box::new(expr)) }
}
