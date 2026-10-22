//! Capture-free CTE names for SQLite's mutually visible sibling bodies.

#[cfg(not(feature = "std"))]
use alloc::{borrow::Cow, collections::BTreeSet, format, string::String, vec, vec::Vec};
use core::ops::ControlFlow;
#[cfg(feature = "std")]
use std::{borrow::Cow, collections::BTreeSet};

use sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, Query, SetExpr, Statement, TableAlias, TableFactor, Visit,
    VisitMut, Visitor, VisitorMut,
};

use crate::impls::{object_name::last_ident, relation_scope::RelationScopes};

pub(super) fn preserve_bindings(query: &Query) -> Cow<'_, Query> {
    let Some(with) = &query.with else { return Cow::Borrowed(query) };
    let mut captures =
        Captures { scopes: RelationScopes::default(), aliases: vec![false; with.cte_tables.len()] };
    let _: ControlFlow<()> = query.visit(&mut captures);
    if !captures.aliases.iter().any(|&captured| captured) {
        return Cow::Borrowed(query);
    }

    let mut names = Names::default();
    let _: ControlFlow<()> = query.visit(&mut names);
    let mut next = 0;
    let replacements = captures
        .aliases
        .into_iter()
        .map(|captured| {
            captured.then(|| {
                loop {
                    let candidate = format!("__pg2sqlite_cte_{next}");
                    next += 1;
                    if !names.0.contains(&candidate) {
                        break candidate;
                    }
                }
            })
        })
        .collect();
    let mut rewritten = query.clone();
    let mut renamer = Renamer { scopes: RelationScopes::default(), replacements };
    let _: ControlFlow<()> = VisitMut::visit(&mut rewritten, &mut renamer);
    Cow::Owned(rewritten)
}

struct Captures {
    scopes: RelationScopes,
    aliases: Vec<bool>,
}

impl Captures {
    fn reference(&mut self, name: &Ident, qualified: bool) {
        let source = if qualified { None } else { self.scopes.binding(name) };
        if source.is_some_and(|(depth, _)| depth > 0) {
            return;
        }
        for (index, alias) in self.scopes.root_aliases().iter().enumerate() {
            if alias.value.eq_ignore_ascii_case(&name.value) && source != Some((0, index)) {
                self.aliases[index] = true;
            }
        }
    }
}

impl Visitor for Captures {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        self.scopes.record_targets(statement);
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.scopes.enter(query);
        if let SetExpr::Table(table) = query.body.as_ref()
            && let Some(name) = &table.table_name
        {
            self.reference(name, table.schema_name.is_some());
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.scopes.exit();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, args: None, .. } = factor
            && !self.scopes.is_target(name)
            && let Some(ident) = last_ident(name)
        {
            self.reference(ident, name.0.len() != 1);
        }
        ControlFlow::Continue(())
    }
}

#[derive(Default)]
struct Names(BTreeSet<String>);

impl Visitor for Names {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.0.insert(cte.alias.name.value.to_ascii_lowercase());
            }
        }
        if let SetExpr::Table(table) = query.body.as_ref()
            && let Some(name) = &table.table_name
        {
            self.0.insert(name.value.to_ascii_lowercase());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<Self::Break> {
        if let Some(ident) = last_ident(name) {
            self.0.insert(ident.value.to_ascii_lowercase());
        }
        ControlFlow::Continue(())
    }
}

struct Renamer {
    scopes: RelationScopes,
    replacements: Vec<Option<String>>,
}

impl Renamer {
    fn replacement(&self, name: &Ident) -> Option<&String> {
        let (0, index) = self.scopes.binding(name)? else { return None };
        self.replacements[index].as_ref()
    }
}

impl VisitorMut for Renamer {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<Self::Break> {
        self.scopes.record_targets(statement);
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.enter(query);
        if let SetExpr::Table(table) = query.body.as_mut()
            && table.schema_name.is_none()
            && let Some(name) = &mut table.table_name
            && let Some(replacement) = self.replacement(name)
        {
            name.value.clone_from(replacement);
            name.quote_style = None;
        }
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        if self.scopes.depth() == 1
            && let Some(with) = &mut query.with
        {
            for (cte, replacement) in with.cte_tables.iter_mut().zip(&self.replacements) {
                if let Some(replacement) = replacement {
                    cte.alias.name.value.clone_from(replacement);
                    cte.alias.name.quote_style = None;
                }
            }
        }
        self.scopes.exit();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, alias, args: None, .. } = factor
            && name.0.len() == 1
            && !self.scopes.is_target(name)
            && let ObjectNamePart::Identifier(ident) = &mut name.0[0]
            && let Some(replacement) = self.replacement(ident)
        {
            let written = alias.is_none().then(|| ident.clone());
            ident.value.clone_from(replacement);
            ident.quote_style = None;
            if let Some(written) = written {
                *alias = Some(TableAlias {
                    explicit: true,
                    name: written,
                    columns: Vec::new(),
                    at: None,
                });
            }
        }
        ControlFlow::Continue(())
    }
}
