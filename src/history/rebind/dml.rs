//! Planning of the columns a data-modifying statement names on its target.

#[cfg(not(feature = "std"))]
use alloc::{borrow::ToOwned, vec::Vec};

use sql_traits::{
    structs::{ColumnScope, IdentifierCase, ParserDB},
    traits::{ColumnLike, DatabaseLike, TableLike},
};
use sqlparser::ast::{
    AssignmentTarget, ConflictTarget, Expr, Ident, Insert, OnConflictAction, OnInsert, Query,
    SelectItem, SetExpr, TableAlias, TableFactor, TableObject, TableWithJoins,
};

use super::{
    Header, Planner,
    scope::{Relation, last, resolve},
};
use crate::{
    impls::{query_builder::plain_table_factor, shared_helpers::relations_scope_query},
    script::ConnectionError,
};

impl Planner<'_> {
    /// Plans an `INSERT`, whose target columns, conflict target and
    /// conflict action name the target table's columns, the action also
    /// through the `EXCLUDED` row.
    pub(super) fn insert(&mut self, insert: &Insert) -> Result<(), ConnectionError> {
        if let Some(query) = &insert.source {
            self.root(query, Header::Follow)?;
        }
        let TableObject::TableName(name) = &insert.table else { return Ok(()) };
        let Relation::Table(table) = resolve(self.epoch, name) else { return Ok(()) };
        if table.is_temporary() {
            return Ok(());
        }
        for column in &insert.columns {
            if let Some(ident) = last(column) {
                self.target_column(table, ident)?;
            }
        }
        let mut exprs: Vec<&Expr> = Vec::new();
        if let Some(OnInsert::OnConflict(conflict)) = &insert.on {
            if let Some(ConflictTarget::Columns(columns)) = &conflict.conflict_target {
                for ident in columns {
                    self.target_column(table, ident)?;
                }
            }
            if let OnConflictAction::DoUpdate(update) = &conflict.action {
                for assignment in &update.assignments {
                    self.assignment_target(table, &assignment.target)?;
                    exprs.push(&assignment.value);
                }
                exprs.extend(update.selection.as_ref());
            }
        }
        for item in insert.returning.iter().flatten() {
            if let SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } = item {
                exprs.push(expr);
            }
        }
        if exprs.is_empty() {
            return Ok(());
        }
        let alias =
            |name: Ident| TableAlias { explicit: true, name, columns: Vec::new(), at: None };
        let mut target = plain_table_factor(name.clone());
        let mut excluded = plain_table_factor(name.clone());
        if let TableFactor::Table { alias: written, .. } = &mut target {
            *written =
                insert.table_alias.as_ref().map(|table_alias| alias(table_alias.alias.clone()));
        }
        if let TableFactor::Table { alias: written, .. } = &mut excluded {
            *written = Some(alias(Ident::new("excluded")));
        }
        let scope_query = relations_scope_query(
            [target, excluded]
                .into_iter()
                .map(|relation| TableWithJoins { relation, joins: Vec::new() })
                .collect(),
        );
        self.dml(&scope_query, 2, &exprs)
    }

    /// Renames the columns an assignment writes.
    pub(super) fn assignment_target(
        &mut self,
        table: &<ParserDB as DatabaseLike>::Table,
        target: &AssignmentTarget,
    ) -> Result<(), ConnectionError> {
        let names = match target {
            AssignmentTarget::ColumnName(name) => core::slice::from_ref(name),
            AssignmentTarget::Tuple(names) => names.as_slice(),
        };
        for name in names {
            if let Some(ident) = last(name) {
                self.target_column(table, ident)?;
            }
        }
        Ok(())
    }

    /// Renames one column a statement names on its target table.
    pub(super) fn target_column(
        &mut self,
        table: &<ParserDB as DatabaseLike>::Table,
        ident: &Ident,
    ) -> Result<(), ConnectionError> {
        if self.replays_home(table.table_name()) {
            return Ok(());
        }
        let Ok(Some(column)) = table.column(&ident.value, self.epoch, IdentifierCase::AsWritten)
        else {
            return Ok(());
        };
        let current = self
            .epoch
            .column_identity(column)
            .and_then(|id| self.final_schema.column_by_identity(id))
            .ok_or_else(|| {
                ConnectionError::UnavailableColumn {
                    relation: table.table_name().to_owned(),
                    column: column.column_name().to_owned(),
                    statement: self.position.index(),
                }
            })?;
        if current.column_name() != column.column_name() {
            self.edits.rename(ident.span, current.column_name().to_owned());
        }
        Ok(())
    }

    /// Plans a data-modifying statement's expressions in the scope of the
    /// relations it lists, the first `targets` of which are written tables.
    pub(super) fn dml(
        &mut self,
        scope_query: &Query,
        targets: usize,
        exprs: &[&Expr],
    ) -> Result<(), ConnectionError> {
        let Ok(scope) = ColumnScope::from_query(scope_query, self.epoch, IdentifierCase::AsWritten)
        else {
            return Ok(());
        };
        let SetExpr::Select(select) = scope_query.body.as_ref() else { return Ok(()) };
        let Some(outer) = scope.scope_for_select(select) else { return Ok(()) };
        let mut listed = Vec::new();
        for (index, table) in select.from.iter().enumerate() {
            if index < targets {
                self.list_target(&table.relation, &mut listed)?;
            } else {
                self.list(&table.relation, &mut listed)?;
            }
            for join in &table.joins {
                self.list(&join.relation, &mut listed)?;
            }
        }
        self.frames.push(listed);
        let mut result = Ok(());
        for expr in exprs {
            result = self
                .expr(&scope, &|reference| outer.resolve_column_binding(reference), expr)
                .map(|_| ());
            if result.is_err() {
                break;
            }
        }
        self.frames.pop();
        result
    }
}
