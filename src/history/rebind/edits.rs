//! Applies a rebinding plan to a statement in one mutable pass.

#[cfg(not(feature = "std"))]
use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};
use core::ops::ControlFlow;
#[cfg(feature = "std")]
use std::collections::BTreeMap;

use sqlparser::{
    ast::{Expr, Ident, Select, SelectItem, VisitorMut},
    tokenizer::Span,
};

/// Edits to apply, keyed by the source span of the identifier they change.
#[derive(Default)]
pub(super) struct Edits {
    pub(super) renames: BTreeMap<Span, String>,
    pub(super) qualifiers: BTreeMap<Span, Ident>,
    pub(super) aliases: BTreeMap<Span, Ident>,
    pub(super) expansions: BTreeMap<Span, Vec<SelectItem>>,
}

impl Edits {
    pub(super) fn rename(&mut self, span: Span, name: String) {
        if span != Span::empty() {
            self.renames.insert(span, name);
        }
    }

    pub(super) fn qualify(&mut self, span: Span, qualifier: Ident) {
        if span != Span::empty() {
            self.qualifiers.insert(span, qualifier);
        }
    }

    pub(super) fn alias(&mut self, span: Span, alias: Ident) {
        if span != Span::empty() {
            self.aliases.insert(span, alias);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.renames.is_empty() && self.aliases.is_empty() && self.expansions.is_empty()
    }
}

impl VisitorMut for Edits {
    type Break = ();

    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<()> {
        if let Some(items) = self.expansions.get(&select.select_token.0.span) {
            select.projection.clone_from(items);
        }
        for item in &mut select.projection {
            if let SelectItem::UnnamedExpr(expr) = item
                && let Some(alias) = last_span(expr).and_then(|span| self.aliases.get(&span))
            {
                *item = SelectItem::ExprWithAlias { expr: expr.clone(), alias: alias.clone() };
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        if let Expr::Identifier(ident) = expr
            && let Some(qualifier) = self.qualifiers.get(&ident.span)
        {
            *expr = Expr::CompoundIdentifier(vec![qualifier.clone(), ident.clone()]);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<()> {
        if let Some(name) = self.renames.get(&ident.span) {
            *ident = quoted(name, ident.span);
        }
        ControlFlow::Continue(())
    }
}

/// The span of the identifier naming a bare column reference.
pub(super) fn last_span(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Identifier(ident) => Some(ident.span),
        Expr::CompoundIdentifier(parts) => parts.last().map(|ident| ident.span),
        _ => None,
    }
}

/// A quoted identifier, which keeps a renamed name exactly as stored and
/// reads correctly whether or not the name needs quotes.
pub(super) fn quoted(name: &str, span: Span) -> Ident {
    Ident::with_quote_and_span('"', span, name)
}
