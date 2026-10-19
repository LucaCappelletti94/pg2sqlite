#[cfg(not(feature = "std"))]
use alloc::{vec, vec::Vec};
use core::ops::ControlFlow;

use sqlparser::ast::{
    Assignment, AssignmentTarget, ConflictTarget, Delete, FromTable, Insert, OnConflictAction,
    OnInsert, SelectItem, SelectItemQualifiedWildcardKind, TableObject, TableWithJoins, Update,
    UpdateTableFromKind, VisitMut,
};

use super::{
    Binding, Entry, Frame, Kind, Owner, RewriteFlow, Rewriter, fold, last_ident, last_ident_mut,
    rename_name,
};

impl Rewriter<'_> {
    pub(super) fn rewrite_insert(&mut self, insert: &mut Insert) -> RewriteFlow {
        let mut target = None;
        if let TableObject::TableName(name) = &mut insert.table
            && let Some(folded) = last_ident(name).map(fold)
        {
            let resolved = match self.owner(&folded) {
                Some(Owner::Final(identity))
                    if matches!(identity.kind, Kind::Table | Kind::View) =>
                {
                    rename_name(name, &identity.final_name);
                    Some((identity.index, true))
                }
                Some(Owner::AsIs(id))
                    if matches!(self.ident(id).kind, Kind::Table | Kind::View) =>
                {
                    Some((id, false))
                }
                _ => None,
            };
            if let Some((id, rewrite)) = resolved {
                target = Some((id, rewrite));
                self.frames.push(Frame {
                    bindings: vec![Binding {
                        name: insert
                            .table_alias
                            .as_ref()
                            .map_or(folded, |alias| fold(&alias.alias)),
                        entry: Entry::table(id, rewrite, insert.table_alias.is_some()),
                    }],
                    visible_ctes: None,
                });
            }
        }
        let Some((id, rewrite)) = target else {
            return ControlFlow::Continue(());
        };
        for column in &mut insert.columns {
            if let Some(ident) = last_ident_mut(column) {
                self.rewrite_column_in_scope(ident)?;
            }
        }
        if let Some(source) = &mut insert.source {
            self.visit_query(source)?;
        }
        self.rewrite_assignments(&mut insert.assignments)?;
        match &mut insert.on {
            Some(OnInsert::OnConflict(conflict)) => {
                if let Some(ConflictTarget::Columns(columns)) = &mut conflict.conflict_target {
                    for column in columns {
                        self.rewrite_column_in_scope(column)?;
                    }
                }
                if let OnConflictAction::DoUpdate(update) = &mut conflict.action {
                    self.frames.push(Frame {
                        bindings: vec![Binding {
                            name: "excluded".into(),
                            entry: Entry::QualifiedTable(id, rewrite),
                        }],
                        visible_ctes: None,
                    });
                    self.rewrite_assignments(&mut update.assignments)?;
                    update.selection.visit(self)?;
                    self.pop_frame();
                }
            }
            Some(OnInsert::DuplicateKeyUpdate(assignments)) => {
                self.rewrite_assignments(assignments)?;
            }
            _ => {}
        }
        self.rewrite_returning(&mut insert.returning)?;
        self.pop_frame();
        ControlFlow::Continue(())
    }

    fn rewrite_assignments(&mut self, assignments: &mut [Assignment]) -> RewriteFlow {
        for assignment in assignments {
            match &mut assignment.target {
                AssignmentTarget::ColumnName(column) => {
                    if let Some(ident) = last_ident_mut(column) {
                        self.rewrite_column_in_scope(ident)?;
                    }
                }
                AssignmentTarget::Tuple(columns) => {
                    for column in columns {
                        if let Some(ident) = last_ident_mut(column) {
                            self.rewrite_column_in_scope(ident)?;
                        }
                    }
                }
            }
            assignment.value.visit(self)?;
        }
        ControlFlow::Continue(())
    }

    fn rewrite_returning(&mut self, returning: &mut Option<Vec<SelectItem>>) -> RewriteFlow {
        if let Some(returning) = returning {
            for item in returning {
                item.visit(self)?;
                if let SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) = item
                    && let Some(qualifier) = last_ident(name).map(fold)
                    && let Some(Entry::Table(table, true)) = self.qualified_entry(&qualifier)
                {
                    rename_name(name, &self.final_identity(*table).final_name);
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn bind_table_into(
        &mut self,
        table: &mut TableWithJoins,
        bindings: &mut Vec<Binding>,
    ) -> RewriteFlow {
        self.bind_factor_into(&mut table.relation, bindings)?;
        for join in &mut table.joins {
            self.bind_factor_into(&mut join.relation, bindings)?;
        }
        ControlFlow::Continue(())
    }

    pub(super) fn rewrite_update(&mut self, update: &mut Update) -> RewriteFlow {
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        self.bind_table_into(&mut update.table, &mut frame.bindings)?;
        if let Some(from) = &mut update.from {
            let tables = match from {
                UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables) => {
                    tables
                }
            };
            for table in tables {
                self.bind_table_into(table, &mut frame.bindings)?;
            }
        }
        self.frames.push(frame);
        update.table.visit(self)?;
        update.from.visit(self)?;
        self.rewrite_assignments(&mut update.assignments)?;
        update.selection.visit(self)?;
        update.order_by.visit(self)?;
        update.limit.visit(self)?;
        self.rewrite_returning(&mut update.returning)?;
        self.pop_frame();
        ControlFlow::Continue(())
    }

    pub(super) fn rewrite_delete(&mut self, delete: &mut Delete) -> RewriteFlow {
        let tables = match &mut delete.from {
            FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
        };
        let mut frame = Frame { bindings: Vec::new(), visible_ctes: None };
        for table in tables {
            self.bind_table_into(table, &mut frame.bindings)?;
        }
        if let Some(using) = &mut delete.using {
            for table in using {
                self.bind_table_into(table, &mut frame.bindings)?;
            }
        }
        self.frames.push(frame);
        delete.from.visit(self)?;
        delete.using.visit(self)?;
        delete.selection.visit(self)?;
        delete.order_by.visit(self)?;
        delete.limit.visit(self)?;
        self.rewrite_returning(&mut delete.returning)?;
        self.pop_frame();
        ControlFlow::Continue(())
    }
}
