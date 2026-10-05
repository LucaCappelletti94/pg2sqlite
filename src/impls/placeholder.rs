//! Translates bind placeholders between SQLite and PostgreSQL parameter syntax.
//!
//! SQLite accepts positional `?`, numbered `?N`, and the named forms `:name`,
//! `@name`, and `$name`. PostgreSQL accepts only numbered `$N`. The two
//! directions are asymmetric because PostgreSQL is the stricter dialect:
//!
//! - Reverse (SQLite to PostgreSQL): [`rewrite_placeholders_for_postgres`] maps
//!   `?N` to `$N` and a bare `?` to one greater than the largest number used so
//!   far, following SQLite's own bind-index rule in source order so a single
//!   bind vector drives both sides without reordering. The named forms have no
//!   PostgreSQL equivalent (and diesel never emits them), so they are rejected
//!   with a typed error rather than passed through as SQL that misparses
//!   (`$name` reads as a dollar-quoted string opener).
//! - Forward (PostgreSQL to SQLite): [`rewrite_placeholders_for_sqlite`] maps
//!   `$N` to `?N`, preserving the number. The remaining forms already parse
//!   under SQLite, so they are left untouched.
//!
//! The number is preserved in both directions, so a placeholder survives a
//! round trip without losing its bind index: `?N` and `$N` map to each other
//! byte for byte, and a bare `?` canonicalizes to `?N` on its first round trip
//! (the very index SQLite itself would assign it) and then stays fixed.
//!
//! Reverse assignment follows the source order of the SQL text, matched by the
//! span each placeholder carries rather than the AST walk order, so a bare `?`
//! in any clause (WHERE, IN, BETWEEN, LIMIT, OFFSET, function arguments, or the
//! select list) is numbered by its textual position.

use alloc::collections::BTreeMap;
#[cfg(not(feature = "std"))]
use alloc::{format, string::String, vec::Vec};
use core::{convert::Infallible, ops::ControlFlow};

use sqlparser::{
    ast::{Value, ValueWithSpan, Visit, VisitMut, Visitor, VisitorMut},
    tokenizer::Location,
};

use crate::errors::Error;

// Reverse direction: SQLite placeholders to PostgreSQL `$N` parameters.

/// A placeholder that maps to a PostgreSQL numbered parameter.
#[derive(Clone, Copy)]
enum NumberablePlaceholder {
    /// Bare positional `?`.
    Positional,
    /// Numbered `?N`.
    Numbered(u32),
}

/// Classification of a `Value::Placeholder` token as parsed by the SQLite
/// tokenizer.
enum PlaceholderToken {
    /// `?` or `?N`, the forms that map to PostgreSQL `$N`.
    Numberable(NumberablePlaceholder),
    /// `:name`, `@name`, or `$name`, which PostgreSQL cannot represent.
    Named,
    /// Not a recognized placeholder form. The SQLite tokenizer never emits this
    /// for a `Value::Placeholder`, but keeping the classifier total avoids a
    /// silent panic path.
    Unrecognized,
}

/// Classifies a placeholder token by its leading character. The SQLite
/// tokenizer stores the full token text, e.g. `?`, `?1`, `:name`, `@name`, or
/// `$name`.
fn classify(token: &str) -> PlaceholderToken {
    if let Some(digits) = token.strip_prefix('?') {
        if digits.is_empty() {
            PlaceholderToken::Numberable(NumberablePlaceholder::Positional)
        } else if let Ok(number) = digits.parse::<u32>() {
            PlaceholderToken::Numberable(NumberablePlaceholder::Numbered(number))
        } else {
            PlaceholderToken::Unrecognized
        }
    } else if token.starts_with([':', '@', '$']) {
        PlaceholderToken::Named
    } else {
        PlaceholderToken::Unrecognized
    }
}

/// Collects every numberable placeholder with its source location, breaking on
/// the first named placeholder so the caller can raise a typed error.
struct PlaceholderCollector {
    found: Vec<(Location, NumberablePlaceholder)>,
}

impl Visitor for PlaceholderCollector {
    type Break = String;

    fn pre_visit_value(&mut self, value: &ValueWithSpan) -> ControlFlow<Self::Break> {
        let Value::Placeholder(token) = &value.value else {
            return ControlFlow::Continue(());
        };
        match classify(token) {
            PlaceholderToken::Numberable(kind) => self.found.push((value.span.start, kind)),
            PlaceholderToken::Named => return ControlFlow::Break(token.clone()),
            PlaceholderToken::Unrecognized => {}
        }
        ControlFlow::Continue(())
    }
}

/// Assigns a PostgreSQL `$N` token to each placeholder location following
/// SQLite's bind-index rule in source order: a bare `?` takes one greater than
/// the largest number used so far, and `?N` keeps its number.
fn assign_numbers(found: &[(Location, NumberablePlaceholder)]) -> BTreeMap<Location, String> {
    let mut ordered = found.to_vec();
    ordered.sort_by_key(|(location, _)| *location);

    let mut max = 0u32;
    let mut mapping = BTreeMap::new();
    for (location, kind) in ordered {
        let number = match kind {
            NumberablePlaceholder::Positional => {
                max += 1;
                max
            }
            NumberablePlaceholder::Numbered(number) => {
                max = max.max(number);
                number
            }
        };
        mapping.insert(location, format!("${number}"));
    }
    mapping
}

/// Replaces each placeholder token with its assigned `$N`, matched by source
/// location.
struct PlaceholderRewriter {
    mapping: BTreeMap<Location, String>,
}

impl VisitorMut for PlaceholderRewriter {
    type Break = Infallible;

    fn pre_visit_value(&mut self, value: &mut ValueWithSpan) -> ControlFlow<Self::Break> {
        if let Value::Placeholder(_) = &value.value
            && let Some(replacement) = self.mapping.get(&value.span.start)
        {
            value.value = Value::Placeholder(replacement.clone());
        }
        ControlFlow::Continue(())
    }
}

/// Rewrites every SQLite bind placeholder in `node` to a PostgreSQL numbered
/// parameter, returning [`Error::UnsupportedNamedPlaceholder`] if a named form
/// is present. A placeholder-free node is left byte-identical.
pub(crate) fn rewrite_placeholders_for_postgres<N: Visit + VisitMut>(
    node: &mut N,
) -> Result<(), Error> {
    let mut collector = PlaceholderCollector { found: Vec::new() };
    if let ControlFlow::Break(placeholder) = Visit::visit(&*node, &mut collector) {
        return Err(Error::UnsupportedNamedPlaceholder { placeholder });
    }
    if collector.found.is_empty() {
        return Ok(());
    }
    let mut rewriter = PlaceholderRewriter { mapping: assign_numbers(&collector.found) };
    let _ = VisitMut::visit(node, &mut rewriter);
    Ok(())
}

// Forward direction: PostgreSQL `$N` parameters to SQLite `?N` placeholders.

/// Returns the digit run of a PostgreSQL numbered parameter token (`$N`), or
/// `None` for any other placeholder form (SQLite accepts those natively).
fn postgres_parameter_digits(token: &str) -> Option<&str> {
    let digits = token.strip_prefix('$')?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(digits)
}

/// Rewrites PostgreSQL `$N` parameters to SQLite `?N` placeholders, keeping the
/// number so the bind index survives the translation.
struct PostgresToSqlite;

impl VisitorMut for PostgresToSqlite {
    type Break = Infallible;

    fn pre_visit_value(&mut self, value: &mut ValueWithSpan) -> ControlFlow<Self::Break> {
        let replacement = match &value.value {
            Value::Placeholder(token) => {
                postgres_parameter_digits(token).map(|digits| format!("?{digits}"))
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            value.value = Value::Placeholder(replacement);
        }
        ControlFlow::Continue(())
    }
}

/// Rewrites every PostgreSQL numbered parameter (`$N`) in `node` to a SQLite
/// numbered placeholder (`?N`), preserving the number so a round trip keeps the
/// bind index. Other placeholder forms are left untouched.
pub(crate) fn rewrite_placeholders_for_sqlite<N: VisitMut>(node: &mut N) {
    let _ = VisitMut::visit(node, &mut PostgresToSqlite);
}
