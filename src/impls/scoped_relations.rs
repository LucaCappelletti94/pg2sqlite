//! Physical relations with lexical common-table-expression bindings excluded.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::{ops::ControlFlow, ptr};

use sql_traits::utils::identifier_resolution::identifiers_match;
use sqlparser::ast::{
    FromTable, Ident, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor,
};

use crate::impls::object_name::{last_ident, last_ident_value_or_display};

/// Physical relation names in first-occurrence order, ASCII-folded for SQLite.
#[must_use]
pub(crate) fn relation_names<V: Visit>(node: &V) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for occurrence in relation_occurrences(node) {
        let name = last_ident_value_or_display(&occurrence).to_ascii_lowercase();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every name that denotes stored relations rather than a common-table
/// expression, in source order and with repeats.
#[must_use]
pub(crate) fn relation_occurrences<V: Visit>(node: &V) -> Vec<ObjectName> {
    let mut collector = RelationCollector {
        occurrences: Vec::new(),
        cte_scopes: Vec::new(),
        physical_targets: Vec::new(),
        inside_table_factor: false,
    };
    let _: ControlFlow<()> = node.visit(&mut collector);
    collector.occurrences
}

/// One query's CTE names and their visibility while its children are visited.
struct CteScope {
    aliases: Vec<Ident>,
    body_visibility: Vec<(usize, usize)>,
    visible_aliases: usize,
    restore_parent_visibility: Option<usize>,
}

struct RelationCollector {
    occurrences: Vec<ObjectName>,
    cte_scopes: Vec<CteScope>,
    /// Node identities distinguish write targets from lexical bindings.
    physical_targets: Vec<usize>,
    /// Only a table factor's own relation can bind a common-table expression.
    inside_table_factor: bool,
}

impl RelationCollector {
    fn push_name(&mut self, name: &ObjectName) {
        self.occurrences.push(name.clone());
    }

    /// Whether a one-part reference binds a visible common-table expression.
    fn bound_by_cte(&self, reference: &Ident) -> bool {
        for scope in self.cte_scopes.iter().rev() {
            for alias in scope.aliases.iter().take(scope.visible_aliases) {
                if !alias.value.eq_ignore_ascii_case(&reference.value) {
                    continue;
                }
                if identifiers_match(
                    &alias.value,
                    alias.quote_style.is_some(),
                    &reference.value,
                    reference.quote_style.is_some(),
                ) {
                    return true;
                }
            }
        }
        false
    }

    /// Write targets always name storage.
    fn record_target(&mut self, factor: &TableFactor) {
        if let TableFactor::Table { name, .. } = factor {
            self.physical_targets.push(ptr::from_ref(name).addr());
        }
    }

    fn pop_query_scope(&mut self) {
        let restore = self.cte_scopes.pop().and_then(|scope| scope.restore_parent_visibility);
        if let Some(restore) = restore
            && let Some(parent) = self.cte_scopes.last_mut()
        {
            parent.visible_aliases = restore;
        }
    }
}

impl Visitor for RelationCollector {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        match statement {
            Statement::Update(update) => self.record_target(&update.table.relation),
            Statement::Merge(merge) => self.record_target(&merge.table),
            Statement::Delete(delete) => {
                let tables = match &delete.from {
                    FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
                        tables
                    }
                };
                for entry in tables {
                    self.record_target(&entry.relation);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let address = ptr::from_ref(query).addr();
        let restore_parent_visibility = self.cte_scopes.last_mut().and_then(|scope| {
            scope
                .body_visibility
                .iter()
                .find_map(|(child, visible)| (*child == address).then_some(*visible))
                .map(|visible| {
                    let restore = scope.visible_aliases;
                    scope.visible_aliases = visible;
                    restore
                })
        });
        let (aliases, body_visibility) = query.with.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |with| {
                let recursive = usize::from(with.recursive);
                let aliases =
                    with.cte_tables.iter().map(|cte| cte.alias.name.clone()).collect::<Vec<_>>();
                let body_visibility = with
                    .cte_tables
                    .iter()
                    .enumerate()
                    .map(|(index, cte)| {
                        (ptr::from_ref(cte.query.as_ref()).addr(), index + recursive)
                    })
                    .collect();
                (aliases, body_visibility)
            },
        );
        let visible_aliases = aliases.len();
        self.cte_scopes.push(CteScope {
            aliases,
            body_visibility,
            visible_aliases,
            restore_parent_visibility,
        });

        // Bare TABLE references do not fire the relation hook.
        if let SetExpr::Table(table) = query.body.as_ref()
            && let Some(name) = &table.table_name
            && (table.schema_name.is_some() || !self.bound_by_cte(name))
        {
            let parts = table.schema_name.iter().cloned().chain([name.clone()]);
            self.occurrences.push(ObjectName::from(parts.collect::<Vec<_>>()));
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.pop_query_scope();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        self.inside_table_factor = matches!(factor, TableFactor::Table { .. });
        ControlFlow::Continue(())
    }

    fn post_visit_table_factor(&mut self, _factor: &TableFactor) -> ControlFlow<Self::Break> {
        self.inside_table_factor = false;
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let physical = if self.inside_table_factor && relation.0.len() == 1 {
            let address = ptr::from_ref(relation).addr();
            if self.physical_targets.contains(&address) {
                true
            } else {
                match last_ident(relation) {
                    Some(ident) => !self.bound_by_cte(ident),
                    None => true,
                }
            }
        } else {
            true
        };
        if physical {
            self.push_name(relation);
        }
        ControlFlow::Continue(())
    }
}
