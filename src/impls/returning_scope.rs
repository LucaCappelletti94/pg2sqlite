//! What a RETURNING list may name once the statement reaches SQLite.
//!
//! SQLite's RETURNING sees one row, the one being deleted or updated. It takes
//! a bare column of the target, the target's real table name qualifying a
//! column, `*` meaning that row, and expressions over those. It refuses every
//! `table.*` spelling, every reference to a USING or FROM relation, and every
//! reference qualified by the target's own alias. PostgreSQL takes all five,
//! and on the last one it is the mirror image: after `DELETE FROM t AS a` it
//! requires `a.id` and refuses `t.id`.
//!
//! Two of the five have an exact SQLite spelling and are rewritten here. The
//! rest are refused, including a bare `*` beside a USING or FROM clause, which
//! PostgreSQL expands over those relations as well and which would otherwise
//! answer a silently narrower row.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    Expr, FromTable, Ident, ObjectName, SelectItem, SelectItemQualifiedWildcardKind, TableFactor,
    TableWithJoins,
};

use super::{
    expr_helpers::try_map_expr_children,
    object_name::{last_ident, table_with_implicit_public_lookup},
};
use crate::errors::Error;

/// The first relation a `DELETE` targets, which is the only one its RETURNING
/// list can read.
pub(crate) fn delete_target(from: &FromTable) -> Option<&TableFactor> {
    let (FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables)) = from;
    tables.first().map(|table| &table.relation)
}

/// Rewrites what SQLite spells differently and refuses what it cannot see.
///
/// `auxiliary` is the USING or FROM list, already translated, and `clause` is
/// the keyword that introduced it, which the refusals name.
pub(crate) fn scope_returning_to_target(
    returning: Option<Vec<SelectItem>>,
    target: Option<&TableFactor>,
    auxiliary: &[TableWithJoins],
    schema: &ParserDB,
    clause: &'static str,
) -> Result<Option<Vec<SelectItem>>, Error> {
    let Some(items) = returning else { return Ok(None) };
    let scope = Scope::new(target, auxiliary, schema, clause);
    items.into_iter().map(|item| scope.rewrite_item(item)).collect::<Result<_, _>>().map(Some)
}

/// A relation the statement mentions, as the returned list could name it.
struct Relation<'a> {
    /// The identifier a column reference can qualify with. PostgreSQL hides
    /// the table name behind an alias, so this is the alias when there is one.
    visible: &'a Ident,
    /// The table it reads, which is what the schema is asked about.
    declared: &'a ObjectName,
    /// True when `visible` is an alias. SQLite cannot resolve one in a
    /// returned list, and PostgreSQL insists on it.
    aliased: bool,
}

impl<'a> Relation<'a> {
    fn of(factor: &'a TableFactor) -> Option<Self> {
        let TableFactor::Table { name, alias, .. } = factor else { return None };
        Some(match alias {
            Some(alias) => Self { visible: &alias.name, declared: name, aliased: true },
            None => Self { visible: last_ident(name)?, declared: name, aliased: false },
        })
    }

    fn is_named(&self, ident: &Ident) -> bool {
        self.visible.value.eq_ignore_ascii_case(&ident.value)
    }
}

struct Scope<'a> {
    target: Option<Relation<'a>>,
    /// The target's declared columns, when the schema holds them.
    target_columns: Option<Vec<String>>,
    auxiliary: Vec<Relation<'a>>,
    /// Every declared column of every auxiliary relation, which is what a bare
    /// name is checked against when the target's own columns are unknown.
    auxiliary_columns: Vec<String>,
    clause: &'static str,
}

impl<'a> Scope<'a> {
    fn new(
        target: Option<&'a TableFactor>,
        auxiliary: &'a [TableWithJoins],
        schema: &ParserDB,
        clause: &'static str,
    ) -> Self {
        let target = target.and_then(Relation::of);
        let target_columns =
            target.as_ref().and_then(|relation| declared_columns(relation, schema));

        let auxiliary = auxiliary
            .iter()
            .flat_map(|table| {
                core::iter::once(&table.relation)
                    .chain(table.joins.iter().map(|join| &join.relation))
            })
            .filter_map(Relation::of)
            .collect::<Vec<_>>();
        let auxiliary_columns = auxiliary
            .iter()
            .filter_map(|relation| declared_columns(relation, schema))
            .flatten()
            .collect();

        Self { target, target_columns, auxiliary, auxiliary_columns, clause }
    }

    fn names_the_target(&self, ident: &Ident) -> bool {
        self.target.as_ref().is_some_and(|target| target.is_named(ident))
    }

    fn names_an_auxiliary(&self, ident: &Ident) -> Option<&Relation<'a>> {
        self.auxiliary.iter().find(|relation| relation.is_named(ident))
    }

    /// True when a bare name cannot have come from the target.
    ///
    /// Valid PostgreSQL leaves nowhere else for it once a USING or FROM clause
    /// is present, so a name the target does not declare is one of theirs. When
    /// the target is undeclared its columns are unknown, and the question is
    /// answered the other way round, from the auxiliary relations that are.
    fn is_outside_the_target(&self, name: &str) -> bool {
        if self.auxiliary.is_empty() {
            return false;
        }
        self.target_columns.as_ref().map_or_else(
            || self.auxiliary_columns.iter().any(|column| column.eq_ignore_ascii_case(name)),
            |columns| !columns.iter().any(|column| column.eq_ignore_ascii_case(name)),
        )
    }

    fn rewrite_item(&self, item: SelectItem) -> Result<SelectItem, Error> {
        match item {
            SelectItem::Wildcard(options) if self.auxiliary.is_empty() => {
                Ok(SelectItem::Wildcard(options))
            }
            SelectItem::Wildcard(_) => Err(self.star_error()),
            SelectItem::QualifiedWildcard(kind, options) => {
                let names_target = match &kind {
                    SelectItemQualifiedWildcardKind::ObjectName(name) => {
                        last_ident(name).is_some_and(|ident| self.names_the_target(ident))
                    }
                    SelectItemQualifiedWildcardKind::Expr(_) => false,
                };
                if names_target {
                    // SQLite refuses every `table.*` in a returned list, and
                    // the target's star is exactly its bare `*`.
                    Ok(SelectItem::Wildcard(options))
                } else {
                    Err(self.outside_error(&kind.to_string()))
                }
            }
            SelectItem::UnnamedExpr(expr) => Ok(SelectItem::UnnamedExpr(self.rewrite_expr(&expr)?)),
            SelectItem::ExprWithAlias { expr, alias } => {
                Ok(SelectItem::ExprWithAlias { expr: self.rewrite_expr(&expr)?, alias })
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                Ok(SelectItem::ExprWithAliases { expr: self.rewrite_expr(&expr)?, aliases })
            }
        }
    }

    fn rewrite_expr(&self, expr: &Expr) -> Result<Expr, Error> {
        match expr {
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                let qualifier = &parts[parts.len() - 2];
                if self.names_an_auxiliary(qualifier).is_some() {
                    return Err(self.outside_error(&expr.to_string()));
                }
                if parts.len() == 2
                    && self.target.as_ref().is_some_and(|target| target.aliased)
                    && self.names_the_target(qualifier)
                {
                    return Ok(Expr::Identifier(parts[1].clone()));
                }
                Ok(expr.clone())
            }
            Expr::Identifier(ident) if self.is_outside_the_target(&ident.value) => {
                Err(self.outside_error(&ident.value))
            }
            // A subquery carries its own relations, and a correlated reference
            // back to the target resolves, so the walk stops at its boundary.
            _ => {
                try_map_expr_children(expr, &|child| self.rewrite_expr(child), &|query| {
                    Ok(query.clone())
                })
            }
        }
    }

    fn target_name(&self) -> String {
        self.target
            .as_ref()
            .map_or_else(|| "the target table".to_string(), |target| target.visible.value.clone())
    }

    fn outside_error(&self, reference: &str) -> Error {
        Error::UnsupportedSQLiteFeature(format!(
            "RETURNING {reference} cannot be translated. SQLite's RETURNING sees only the row \
             being changed in {}, so the {} relations are out of scope. Return columns of {} \
             alone, and read the other relation with a separate SELECT.",
            self.target_name(),
            self.clause,
            self.target_name(),
        ))
    }

    fn star_error(&self) -> Error {
        Error::UnsupportedSQLiteFeature(format!(
            "RETURNING * cannot be translated beside a {} clause. PostgreSQL expands it over the \
             {} relations as well, while SQLite's RETURNING * is the changed row in {} alone, so \
             the emitted statement would answer a narrower row than the source. Name the columns \
             to return.",
            self.clause,
            self.clause,
            self.target_name(),
        ))
    }
}

/// The declared column names of `relation`, when the schema holds the table.
fn declared_columns(relation: &Relation<'_>, schema: &ParserDB) -> Option<Vec<String>> {
    let table = table_with_implicit_public_lookup(schema, relation.declared).ok()??;
    Some(table.columns(schema).ok()?.map(|column| column.column_name().to_string()).collect())
}
