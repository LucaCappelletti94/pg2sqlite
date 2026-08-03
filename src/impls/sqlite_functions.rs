//! The functions SQLite resolves without help, so a call to one may be emitted
//! unchanged.
//!
//! A name the translator does not recognise is refused rather than copied into
//! the output, because SQLite answers `no such function` for it at run time,
//! which is a failure the caller sees long after translation succeeded. This
//! inventory is what makes that refusal safe: everything listed here is a
//! function the destination has.
//!
//! Deliberately absent:
//!
//! - The math functions (`sqrt`, `pow`, `ln`, and friends), which ship only
//!   under `SQLITE_ENABLE_MATH_FUNCTIONS`. They are gated on
//!   `with_math_functions_available` earlier in the classification and must not
//!   be admitted here, which would bypass the opt-in.
//! - Extension functions, which arrive with the extension's own option:
//!   SQLiteGIS through its catalog, and anything else through
//!   `with_user_defined_functions`.
//!
//! Names are lower-cased and sorted, and looked up by binary search, because
//! SQLite resolves function names without regard to case.

/// Core scalar, aggregate, window, date, and JSON functions, sorted.
///
/// JSON is here unconditionally because SQLite builds it in by default from
/// 3.38.0, below the 3.46.0 floor.
const SQLITE_BUILTINS: &[&str] = &[
    "abs",
    "changes",
    "char",
    "coalesce",
    "concat",
    "concat_ws",
    "cume_dist",
    // Keywords rather than calls. `sqlparser` models them as a function with
    // no argument list, which renders bare, and bare is the only form SQLite
    // takes: `current_timestamp()` answers `near "(": syntax error`.
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "datetime",
    "dense_rank",
    "first_value",
    "format",
    "glob",
    "group_concat",
    "hex",
    "if",
    "ifnull",
    "iif",
    "instr",
    "json",
    "json_array",
    "json_array_length",
    "json_each",
    "json_error_position",
    "json_extract",
    "json_group_array",
    "json_group_object",
    "json_insert",
    "json_object",
    "json_patch",
    "json_pretty",
    "json_quote",
    "json_remove",
    "json_replace",
    "json_set",
    "json_tree",
    "json_type",
    "json_valid",
    "jsonb",
    "jsonb_array",
    "jsonb_extract",
    "jsonb_group_array",
    "jsonb_group_object",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_patch",
    "jsonb_remove",
    "jsonb_replace",
    "jsonb_set",
    "julianday",
    "lag",
    "last_insert_rowid",
    "last_value",
    "lead",
    "length",
    "like",
    "likelihood",
    "likely",
    "lower",
    "ltrim",
    "max",
    "min",
    "nth_value",
    "ntile",
    "nullif",
    "octet_length",
    "percent_rank",
    "printf",
    "quote",
    // Trigger-only, but `sqlparser` parses it as a function call and the RLS
    // trigger bodies this crate emits are full of it.
    "raise",
    "random",
    "randomblob",
    "rank",
    "replace",
    "round",
    "row_number",
    "rtrim",
    "sign",
    "soundex",
    "sqlite_source_id",
    "sqlite_version",
    "strftime",
    "string_agg",
    "substr",
    "substring",
    "sum",
    "time",
    "timediff",
    "total",
    "total_changes",
    "trim",
    "typeof",
    "unhex",
    "unicode",
    "unixepoch",
    "unlikely",
    "upper",
    "zeroblob",
];

/// Aggregates whose names collide with scalars, kept separate only for the
/// reader: `avg` and `count` have no scalar form.
const SQLITE_AGGREGATES: &[&str] = &["avg", "count"];

/// Names this translator emits itself, which therefore have to survive a round
/// back through the classifier.
///
/// `vec_f32` and `vec_f16` come from the pgvector lowering in
/// [`crate::impls::translator_impls::vector`], and the sqlite-vec extension
/// provides them wherever a vector column is usable at all.
const TRANSLATOR_EMITTED: &[&str] = &["vec_f16", "vec_f32"];

/// Whether SQLite resolves `name` without an extension or an opt-in.
///
/// `name` must already be lower-cased, which every caller does when it reads
/// the identifier.
#[must_use]
pub(crate) fn is_sqlite_builtin(name: &str) -> bool {
    SQLITE_BUILTINS.binary_search(&name).is_ok()
        || SQLITE_AGGREGATES.binary_search(&name).is_ok()
        || TRANSLATOR_EMITTED.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{SQLITE_AGGREGATES, SQLITE_BUILTINS, TRANSLATOR_EMITTED, is_sqlite_builtin};

    /// Binary search answers nonsense on an unsorted slice, and the failure
    /// would be a silently missing name rather than a panic.
    #[test]
    fn every_inventory_is_sorted_and_unique() {
        for inventory in [SQLITE_BUILTINS, SQLITE_AGGREGATES, TRANSLATOR_EMITTED] {
            let mut sorted = inventory.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.as_slice(), inventory, "the inventory must be sorted and unique");
        }
    }

    #[test]
    fn the_math_functions_are_not_admitted_here() {
        for gated in ["sqrt", "pow", "power", "ln", "exp", "log", "log10", "cbrt"] {
            assert!(
                !is_sqlite_builtin(gated),
                "{gated} needs SQLITE_ENABLE_MATH_FUNCTIONS and its own opt-in"
            );
        }
    }
}
