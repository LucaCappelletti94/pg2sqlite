//! Lexical CTE visibility and physical write targets in PostgreSQL queries.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ptr;

use sql_traits::utils::identifier_resolution::identifiers_match;
use sqlparser::ast::{FromTable, Ident, ObjectName, Query, Statement, TableFactor};

#[derive(Default)]
pub(crate) struct RelationScopes {
    scopes: Vec<CteScope>,
    targets: Vec<usize>,
}

struct CteScope {
    aliases: Vec<Ident>,
    bodies: Vec<(usize, usize)>,
    visible: usize,
    restore: Option<usize>,
}

impl RelationScopes {
    pub(crate) fn enter(&mut self, query: &Query) {
        let address = ptr::from_ref(query).addr();
        let restore = self.scopes.last_mut().and_then(|scope| {
            scope
                .bodies
                .iter()
                .find_map(|(child, visible)| (*child == address).then_some(*visible))
                .map(|visible| core::mem::replace(&mut scope.visible, visible))
        });
        let (aliases, bodies) = query.with.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |with| {
                let aliases = with.cte_tables.iter().map(|cte| cte.alias.name.clone()).collect();
                let bodies = with
                    .cte_tables
                    .iter()
                    .enumerate()
                    .map(|(index, cte)| {
                        let visible = if with.recursive { with.cte_tables.len() } else { index };
                        (ptr::from_ref(cte.query.as_ref()).addr(), visible)
                    })
                    .collect();
                (aliases, bodies)
            },
        );
        let visible = aliases.len();
        self.scopes.push(CteScope { aliases, bodies, visible, restore });
    }

    pub(crate) fn exit(&mut self) {
        let restore = self.scopes.pop().and_then(|scope| scope.restore);
        if let Some(visible) = restore
            && let Some(parent) = self.scopes.last_mut()
        {
            parent.visible = visible;
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.scopes.len()
    }

    pub(crate) fn binding(&self, reference: &Ident) -> Option<(usize, usize)> {
        self.scopes.iter().enumerate().rev().find_map(|(depth, scope)| {
            scope.aliases[..scope.visible].iter().enumerate().find_map(|(index, alias)| {
                identifiers_match(
                    &alias.value,
                    alias.quote_style.is_some(),
                    &reference.value,
                    reference.quote_style.is_some(),
                )
                .then_some((depth, index))
            })
        })
    }

    pub(crate) fn root_aliases(&self) -> &[Ident] {
        self.scopes.first().map_or(&[], |scope| scope.aliases.as_slice())
    }

    pub(crate) fn record_targets(&mut self, statement: &Statement) {
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
    }

    fn record_target(&mut self, factor: &TableFactor) {
        if let TableFactor::Table { name, .. } = factor {
            self.targets.push(ptr::from_ref(name).addr());
        }
    }

    pub(crate) fn is_target(&self, name: &ObjectName) -> bool {
        self.targets.contains(&ptr::from_ref(name).addr())
    }
}
