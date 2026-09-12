//! Whether an expression may be written into more than one output position.
//!
//! Several lowerings name an operand twice: a guard plus a body, a sign test
//! plus a length, a prefix plus a suffix. PostgreSQL evaluates the operand
//! once, so the copies agree only while the operand answers the same value
//! every time it runs. A `random()`, a UUID call or a function this crate
//! cannot identify breaks that, and the emitted SQL then answers from a draw
//! PostgreSQL never made.

use alloc::{format, string::String, vec::Vec};
use core::ops::ControlFlow;

use sqlparser::ast::{Expr, Function, Visit, Visitor};

use crate::{errors::Error, impls::sqlite_functions::classify, options::TranslationContext};

/// The calls that answer a different value each time they run.
///
/// Sorted, and searched rather than matched. A clock function is absent on
/// purpose: `now()` and `current_timestamp` are fixed for the whole statement
/// in both engines, so naming one twice reads the same value twice.
/// `clock_timestamp` and `timeofday` are the two PostgreSQL spellings that
/// are not, and the sequence functions move a counter on every call.
const VOLATILE: &[&str] = &[
    "clock_timestamp",
    "currval",
    "gen_random_bytes",
    "gen_random_uuid",
    "lastval",
    "nextval",
    "pg_backend_pid",
    "random",
    "random_bytes",
    "random_normal",
    "randomblob",
    "setval",
    "statement_timestamp",
    "timeofday",
    "txid_current",
    "uuid_generate_v1",
    "uuid_generate_v4",
    "uuid_generate_v7",
    "uuidv4",
    "uuidv7",
];

/// True when evaluating `expr` a second time must answer what the first
/// evaluation answered.
///
/// Columns, literals and the operators over them replay; a call replays only
/// when this crate can name the function and knows it is not volatile, since
/// a function it cannot name may be anything the caller defined. A subquery
/// never replays: it may read rows another copy of the same statement has
/// already changed.
pub(crate) fn is_replayable(expr: &Expr, options: &TranslationContext<'_>) -> bool {
    let mut check = ReplayCheck { options, replayable: true };
    let _: ControlFlow<()> = expr.visit(&mut check);
    check.replayable
}

/// Refuses a lowering that would name `operand` in more than one output
/// position.
pub(crate) fn reject_duplicated_operand(construct: &str, operand: &Expr) -> Error {
    Error::forward_refusal(format!(
        "{construct} has no SQLite form that reads {operand} once: the emitted expression names \
         it in more than one place, and this operand may answer a different value on each \
         evaluation, so the copies would disagree where PostgreSQL evaluates it once. Read the \
         value into a column or a variable first and name that, or compute {construct} in the \
         application."
    ))
}

struct ReplayCheck<'a, 'o> {
    options: &'a TranslationContext<'o>,
    replayable: bool,
}

impl ReplayCheck<'_, '_> {
    /// True when the call names a function this crate knows and knows to be
    /// deterministic.
    fn call_replays(&self, function: &Function) -> bool {
        let Some(name) = crate::impls::object_name::last_ident(&function.name) else {
            return false;
        };
        let name = name.value.to_ascii_lowercase();
        if VOLATILE.binary_search(&name.as_str()).is_ok() {
            return false;
        }
        if self.uuid_names().iter().any(|uuid| uuid.eq_ignore_ascii_case(&name)) {
            return false;
        }
        classify(&name).is_known()
    }

    /// The UUID function names this translation emits, which a caller names
    /// through the options and which answer a fresh value per call.
    fn uuid_names(&self) -> Vec<String> {
        let mut names = Vec::with_capacity(2);
        names.push(String::from(self.options.get_uuid_function_name()));
        if let Some(v7) = self.options.get_uuid_v7_function_name() {
            names.push(String::from(v7));
        }
        names
    }
}

impl Visitor for ReplayCheck<'_, '_> {
    type Break = ();

    fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let replays = match expr {
            Expr::Function(function) => self.call_replays(function),
            Expr::Subquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => false,
            _ => true,
        };
        if replays {
            ControlFlow::Continue(())
        } else {
            self.replayable = false;
            ControlFlow::Break(())
        }
    }
}
