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

/// What SQLite ships only under `SQLITE_ENABLE_MATH_FUNCTIONS`, which is why
/// none of them is in the inventory above.
///
/// The build flag is a fact about the destination, so only the caller can say
/// whether it holds. `with_math_functions_available` is that claim, and every
/// name here is emittable once it is made. All but `log2` are also PostgreSQL
/// names, measured against its catalogue, which is why `SHARED_WITH_POSTGRES`
/// carries them and `log2` sits in the reverse translator's `SQLITE_ONLY`
/// instead.
const SQLITE_MATH: &[&str] = &[
    "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "ceil", "ceiling", "cos", "cosh",
    "degrees", "exp", "floor", "ln", "log", "log10", "log2", "mod", "pi", "pow", "power",
    "radians", "sin", "sinh", "sqrt", "tan", "tanh", "trunc",
];

/// Every classification the inventories answer for one name, computed in one
/// place so callers cannot mix lookups from different vintages of the lists.
///
/// The classes overlap by design: a name SQLite has can also be one PostgreSQL
/// shares, and the relations between the lists are pinned by the unit tests
/// below, not by this struct.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NameClass {
    /// SQLite resolves the name without an extension or an opt-in.
    pub sqlite_builtin: bool,
    /// SQLite answers the name only under `SQLITE_ENABLE_MATH_FUNCTIONS`.
    pub gated_math: bool,
    /// Both engines answer the name the same way, so the reverse direction
    /// may emit it unchanged.
    pub shared_with_postgres: bool,
    /// PostgreSQL answers the name and SQLite does not.
    pub postgres_only: bool,
}

/// Classify `name` against every inventory in this module.
///
/// `name` must already be lower-cased, which every caller does when it reads
/// the identifier.
#[must_use]
pub(crate) fn classify(name: &str) -> NameClass {
    NameClass {
        sqlite_builtin: SQLITE_BUILTINS.binary_search(&name).is_ok()
            || SQLITE_AGGREGATES.binary_search(&name).is_ok()
            || TRANSLATOR_EMITTED.binary_search(&name).is_ok(),
        gated_math: SQLITE_MATH.binary_search(&name).is_ok(),
        shared_with_postgres: SHARED_WITH_POSTGRES.binary_search(&name).is_ok(),
        postgres_only: POSTGRES_ONLY.binary_search(&name).is_ok(),
    }
}

/// Every name the maths build flag decides, exposed so a test can walk the same
/// list the gate consults.
#[cfg(feature = "std")]
#[must_use]
pub fn gated_math() -> &'static [&'static str] {
    SQLITE_MATH
}

/// Whether SQLite resolves `name`, exposed so a test can ask this crate the
/// question the sweep in `tests/gauntlet/reverse.rs` has to answer: a name both
/// engines have is a judgement about meaning, decided name by name, and not
/// something a catalogue sweep may rule on.
///
/// `name` must already be lower-cased.
#[cfg(feature = "std")]
#[must_use]
pub fn sqlite_has(name: &str) -> bool {
    classify(name).sqlite_builtin
}

/// Every name SQLite answers, the unconditional inventory and the gated maths
/// one together, exposed so a test can walk the corpus the reverse direction
/// has to cope with rather than keep a copy of it.
///
/// `TRANSLATOR_EMITTED` is deliberately absent: those arrive with sqlite-vec
/// rather than with SQLite, so they say nothing about what a plain destination
/// answers.
#[cfg(feature = "std")]
pub fn sqlite_names() -> impl Iterator<Item = &'static str> {
    SQLITE_BUILTINS.iter().chain(SQLITE_AGGREGATES).chain(SQLITE_MATH).copied()
}

/// SQLite names PostgreSQL answers the same way, which are therefore the only
/// ones the reverse direction may emit unchanged.
///
/// Existence was measured against PostgreSQL 17's own `pg_catalog` rather than
/// recalled, and `tests/gauntlet/reverse.rs` re-measures it against the running
/// server so a name cannot be added here on faith. Two groups the catalogue
/// query alone would have missed are in the list all the same: `coalesce` and
/// `nullif`, which PostgreSQL parses as expressions rather than functions, and
/// the three `current_*` keywords, which both engines take bare.
///
/// The gated math functions are here too. SQLite ships them only under
/// `SQLITE_ENABLE_MATH_FUNCTIONS`, which is why they are absent from the
/// inventory above, but the build flag is a fact about the replica and says
/// nothing about the server: every one of them exists in PostgreSQL, measured,
/// except `log2`, which is refused.
///
/// Agreement of meaning is a separate judgement from existence, and the names
/// that exist in both with different arguments are refused rather than listed:
/// `format` is `printf` in SQLite and `%I`/`%L` templating in PostgreSQL, the
/// `jsonb_` family takes a JSONPath string here and a `text[]` there, and
/// `like` is a reserved word PostgreSQL will not take as a bare call.
const SHARED_WITH_POSTGRES: &[&str] = &[
    "abs",
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "avg",
    "ceil",
    "ceiling",
    "coalesce",
    "concat",
    "concat_ws",
    "cos",
    "cosh",
    "count",
    "cume_dist",
    "current_date",
    "current_time",
    "current_timestamp",
    "degrees",
    "dense_rank",
    "exp",
    "first_value",
    "floor",
    "lag",
    "last_value",
    "lead",
    "length",
    "ln",
    "log",
    "log10",
    "lower",
    "mod",
    "nth_value",
    "ntile",
    "nullif",
    "octet_length",
    "percent_rank",
    "pi",
    "pow",
    "power",
    "radians",
    "rank",
    "replace",
    "round",
    "row_number",
    "sign",
    "sin",
    "sinh",
    "sqrt",
    "string_agg",
    "substr",
    "substring",
    "sum",
    "tan",
    "tanh",
    "trunc",
    "upper",
];

/// Every name this crate claims both engines answer the same way, exposed so
/// the claim can be put to a real PostgreSQL rather than trusted.
///
/// `tests/gauntlet/reverse.rs` reads it and asks the server's own catalogue.
#[cfg(feature = "std")]
#[must_use]
pub fn shared_with_postgres() -> &'static [&'static str] {
    SHARED_WITH_POSTGRES
}

/// Names PostgreSQL answers and SQLite does not, which the reverse direction
/// may therefore emit unchanged.
///
/// The requirement is one-sided. Reverse translation promises that PostgreSQL
/// takes its output, so the only question a name raises is whether the server
/// answers it. Whether SQLite could have produced the name is not something
/// this direction can police, since it never checks that the input's tables or
/// column types exist either, and a source database may carry any extension.
/// Gating the passthrough on SQLite membership is what refused `var_pop` and
/// around 150 further names PostgreSQL has.
///
/// Every entry is absent from SQLite, which is the invariant that keeps a
/// meaning clash out: with no SQLite definition to disagree with, PostgreSQL's
/// meaning is the only one the name can carry. A name both engines have, such
/// as `format` or `jsonb_set`, is a judgement about meaning rather than
/// existence and belongs above, or in the reverse translator's `SQLITE_ONLY`
/// when the meanings differ.
///
/// The list is bounded by the names this crate already knows on the PostgreSQL
/// side: what the forward direction matches on, plus what the reverse direction
/// emits. `tests/gauntlet/reverse.rs` asks the running server about every
/// entry, so nothing here rests on recall. Absent for that reason, measured
/// against the pinned `postgres:18-alpine`: `uuid_generate_v4` needs the
/// `uuid-ossp` extension, `multirange_agg` does not exist, and `truncate` names
/// a statement rather than a function. `uuidv4` and `uuidv7` are present, since
/// 18 introduced them and 18 is the pin.
const POSTGRES_ONLY: &[&str] = &[
    "abbrev",
    "age",
    "any_value",
    "array_agg",
    "array_append",
    "array_cat",
    "array_dims",
    "array_fill",
    "array_length",
    "array_lower",
    "array_ndims",
    "array_position",
    "array_positions",
    "array_prepend",
    "array_remove",
    "array_replace",
    "array_to_string",
    "array_upper",
    "ascii",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bool_and",
    "bool_or",
    "broadcast",
    "btrim",
    "cardinality",
    "cbrt",
    "char_length",
    "character_length",
    "chr",
    "clock_timestamp",
    "col_description",
    "convert",
    "convert_from",
    "convert_to",
    "corr",
    "covar_pop",
    "covar_samp",
    "current_database",
    "current_schema",
    "current_schemas",
    "currval",
    "date_part",
    "date_trunc",
    "decode",
    "div",
    "encode",
    "every",
    "factorial",
    "family",
    "gcd",
    "gen_random_uuid",
    "generate_series",
    "greatest",
    "has_column_privilege",
    "has_database_privilege",
    "has_function_privilege",
    "has_schema_privilege",
    "has_sequence_privilege",
    "has_table_privilege",
    "host",
    "hostmask",
    "initcap",
    "isfinite",
    "json_agg",
    "json_agg_strict",
    "json_array_elements",
    "json_array_elements_text",
    "json_build_array",
    "json_build_object",
    "json_each_text",
    "json_extract_path",
    "json_extract_path_text",
    "json_object_agg",
    "json_object_agg_strict",
    "json_object_agg_unique",
    "json_object_agg_unique_strict",
    "json_object_keys",
    "json_populate_record",
    "json_strip_nulls",
    "json_to_record",
    "json_typeof",
    "jsonb_agg",
    "jsonb_agg_strict",
    "jsonb_array_elements",
    "jsonb_array_elements_text",
    "jsonb_array_length",
    "jsonb_build_array",
    "jsonb_build_object",
    "jsonb_each",
    "jsonb_each_text",
    "jsonb_extract_path",
    "jsonb_extract_path_text",
    "jsonb_object_agg",
    "jsonb_object_agg_strict",
    "jsonb_object_agg_unique",
    "jsonb_object_agg_unique_strict",
    "jsonb_object_keys",
    "jsonb_populate_record",
    "jsonb_strip_nulls",
    "jsonb_to_record",
    "jsonb_typeof",
    "justify_days",
    "justify_hours",
    "justify_interval",
    "lastval",
    "lcm",
    "least",
    "left",
    "localtime",
    "localtimestamp",
    "lpad",
    "make_date",
    "make_interval",
    "make_time",
    "make_timestamp",
    "make_timestamptz",
    "masklen",
    "md5",
    "mode",
    "netmask",
    "network",
    "nextval",
    "now",
    "obj_description",
    "percentile_cont",
    "percentile_disc",
    "pg_column_size",
    "pg_database_size",
    "pg_get_constraintdef",
    "pg_get_expr",
    "pg_get_indexdef",
    "pg_get_viewdef",
    "pg_relation_size",
    "pg_table_size",
    "pg_tablespace_size",
    "pg_total_relation_size",
    "pg_typeof",
    "quote_ident",
    "quote_literal",
    "quote_nullable",
    "range_agg",
    "range_intersect_agg",
    "regexp_match",
    "regexp_matches",
    "regexp_replace",
    "regexp_split_to_array",
    "regexp_split_to_table",
    "regr_avgx",
    "regr_avgy",
    "regr_count",
    "regr_intercept",
    "regr_r2",
    "regr_slope",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    "repeat",
    "reverse",
    "right",
    "row",
    "row_to_json",
    "rpad",
    "set_masklen",
    "setseed",
    "setval",
    "shobj_description",
    "split_part",
    "statement_timestamp",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "string_to_array",
    "strpos",
    "timeofday",
    "to_char",
    "to_date",
    "to_json",
    "to_jsonb",
    "to_number",
    "to_timestamp",
    "transaction_timestamp",
    "translate",
    "ts_rank",
    "ts_rank_cd",
    "unnest",
    "uuidv4",
    "uuidv7",
    "var_pop",
    "var_samp",
    "variance",
    "version",
    "width_bucket",
    "xmlagg",
];

/// Every name this crate claims PostgreSQL has and SQLite lacks, exposed for
/// the same reason as [`shared_with_postgres`].
#[cfg(feature = "std")]
#[must_use]
pub fn postgres_only() -> &'static [&'static str] {
    POSTGRES_ONLY
}

#[cfg(test)]
mod tests {
    use super::{
        POSTGRES_ONLY, SHARED_WITH_POSTGRES, SQLITE_AGGREGATES, SQLITE_BUILTINS, SQLITE_MATH,
        TRANSLATOR_EMITTED, classify,
    };

    /// Binary search answers nonsense on an unsorted slice, and the failure
    /// would be a silently missing name rather than a panic.
    #[test]
    fn every_inventory_is_sorted_and_unique() {
        for inventory in [
            SQLITE_BUILTINS,
            SQLITE_AGGREGATES,
            TRANSLATOR_EMITTED,
            SHARED_WITH_POSTGRES,
            POSTGRES_ONLY,
            SQLITE_MATH,
        ] {
            let mut sorted = inventory.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.as_slice(), inventory, "the inventory must be sorted and unique");
        }
    }

    /// A shared name SQLite does not have at all would be a claim about
    /// nothing. The gated names count, since the build flag says nothing about
    /// what PostgreSQL has.
    #[test]
    fn every_shared_name_is_a_sqlite_name() {
        for name in SHARED_WITH_POSTGRES {
            assert!(
                classify(name).sqlite_builtin || SQLITE_MATH.contains(name),
                "{name} is claimed shared but is not a SQLite name"
            );
        }
    }

    #[test]
    fn the_math_functions_are_not_admitted_here() {
        for gated in SQLITE_MATH.iter().chain(["cbrt"].iter()) {
            assert!(
                !classify(gated).sqlite_builtin,
                "{gated} needs SQLITE_ENABLE_MATH_FUNCTIONS and its own opt-in"
            );
        }
    }

    /// The invariant that keeps a meaning clash out of the PostgreSQL-only
    /// inventory. A name SQLite also has cannot be listed here, because then
    /// the two engines' readings of it would both be live and the passthrough
    /// would pick one silently.
    #[test]
    fn every_postgres_only_name_is_absent_from_sqlite() {
        for name in POSTGRES_ONLY {
            assert!(
                !classify(name).sqlite_builtin && !SQLITE_MATH.contains(name),
                "{name} is a SQLite name, so whether the two engines agree on it is a judgement \
                 that belongs in SHARED_WITH_POSTGRES or in the reverse translator's SQLITE_ONLY"
            );
        }
    }

    /// Two lists the reverse direction reads in one condition, so an overlap
    /// would make one of them dead in that spot.
    #[test]
    fn the_two_postgres_inventories_are_disjoint() {
        for name in POSTGRES_ONLY {
            assert!(
                SHARED_WITH_POSTGRES.binary_search(name).is_err(),
                "{name} is in both PostgreSQL inventories"
            );
        }
    }
}
