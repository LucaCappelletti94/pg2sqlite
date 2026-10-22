//! Physical relations with lexical common-table-expression bindings excluded.

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};
use core::ops::ControlFlow;

use sqlparser::ast::{Ident, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor};

use crate::impls::{
    object_name::{last_ident, last_ident_value_or_display},
    relation_scope::RelationScopes,
};

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
        scopes: RelationScopes::default(),
        inside_table_factor: false,
    };
    let _: ControlFlow<()> = node.visit(&mut collector);
    collector.occurrences
}

struct RelationCollector {
    occurrences: Vec<ObjectName>,
    scopes: RelationScopes,
    /// Only a table factor's own relation can bind a common-table expression.
    inside_table_factor: bool,
}

impl RelationCollector {
    fn push_name(&mut self, name: &ObjectName) {
        self.occurrences.push(name.clone());
    }

    /// Whether a one-part reference binds a visible common-table expression.
    fn bound_by_cte(&self, reference: &Ident) -> bool {
        self.scopes.binding(reference).is_some()
    }
}

impl Visitor for RelationCollector {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        self.scopes.record_targets(statement);
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.scopes.enter(query);

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
        self.scopes.exit();
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
            if self.scopes.is_target(relation) {
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
