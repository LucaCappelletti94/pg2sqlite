//! PostGIS-equivalent function catalog mirrored from the SQLiteGIS extension
//! (<https://github.com/LucaCappelletti94/sqlitegis>, published as `sqlitegis`
//! on crates.io).
//!
//! Used by the function dispatcher in `function.rs` to decide whether an
//! `ST_*`-shaped call should pass through unchanged (when
//! [`crate::traits::TranslationOptions::with_sqlitegis_enabled`] is on) or
//! fail with a precise `UnsupportedSQLiteFeature` error.
//!
//! The list MUST stay in sync with SQLiteGIS's
//! `sqlitegis::core::function_catalog::SQLITE_DETERMINISTIC_FUNCTIONS` and
//! `SQLITE_DIRECT_ONLY_FUNCTIONS`. Drift is caught by the parity tests in
//! `tests/test_postgis_diesel.rs`, which assert that every SQLiteGIS catalog
//! entry has a matching `(name, arity)` here.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

/// `(lowercase-name, arity)` pairs implemented by SQLiteGIS v0.1.0.
///
/// Multiple entries with the same name encode overloaded arities (for
/// example `st_point` takes either 2 or 3 args).
pub(crate) const POSTGIS_FUNCTION_CATALOG: &[(&str, i32)] = &[
    // -- I/O --
    ("st_geomfromtext", 1),
    ("st_geomfromtext", 2),
    ("st_geomfromwkb", 1),
    ("st_geomfromwkb", 2),
    ("st_geomfromewkb", 1),
    ("st_geomfromgeojson", 1),
    ("st_astext", 1),
    ("st_asewkt", 1),
    ("st_asbinary", 1),
    ("st_asewkb", 1),
    ("st_asgeojson", 1),
    // -- Constructors --
    ("st_point", 2),
    ("st_point", 3),
    ("st_makepoint", 2),
    ("st_makeline", 2),
    ("st_makepolygon", 1),
    ("st_makeenvelope", 4),
    ("st_makeenvelope", 5),
    ("st_collect", 2),
    ("st_tileenvelope", 3),
    // -- SRID / type / dimensions --
    ("st_srid", 1),
    ("st_setsrid", 2),
    ("st_geometrytype", 1),
    ("geometrytype", 1),
    ("st_ndims", 1),
    ("st_coorddim", 1),
    ("st_zmflag", 1),
    ("st_isempty", 1),
    ("st_memsize", 1),
    // -- Coordinate accessors --
    ("st_x", 1),
    ("st_y", 1),
    ("st_z", 1),
    // -- Cardinality --
    ("st_numpoints", 1),
    ("st_npoints", 1),
    ("st_numgeometries", 1),
    ("st_numinteriorrings", 1),
    ("st_numinteriorring", 1),
    ("st_numrings", 1),
    // -- Element extraction --
    ("st_pointn", 2),
    ("st_startpoint", 1),
    ("st_endpoint", 1),
    ("st_exteriorring", 1),
    ("st_interiorringn", 2),
    ("st_geometryn", 2),
    // -- Metadata --
    ("st_dimension", 1),
    ("st_envelope", 1),
    ("st_isvalid", 1),
    ("st_isvalidreason", 1),
    // -- Measurement --
    ("st_area", 1),
    ("st_length", 1),
    ("st_length2d", 1),
    ("st_perimeter", 1),
    ("st_perimeter2d", 1),
    ("st_distance", 2),
    ("st_centroid", 1),
    ("st_pointonsurface", 1),
    ("st_hausdorffdistance", 2),
    // -- Bounds --
    ("st_xmin", 1),
    ("st_xmax", 1),
    ("st_ymin", 1),
    ("st_ymax", 1),
    // -- Linear referencing (planar) --
    ("st_segmentize", 2),
    ("st_lineinterpolatepoint", 2),
    ("st_lineinterpolatepoints", 2),
    ("st_linesubstring", 3),
    // -- Geodetic (SRID 4326) --
    ("st_distancesphere", 2),
    ("st_distancespheroid", 2),
    ("st_lengthsphere", 1),
    ("st_lengthspheroid", 1),
    ("st_length2dspheroid", 1),
    ("st_areasphere", 1),
    ("st_areaspheroid", 1),
    ("st_perimetersphere", 1),
    ("st_perimeterspheroid", 1),
    ("st_segmentizesphere", 2),
    ("st_segmentizespheroid", 2),
    ("st_lineinterpolatepointsphere", 2),
    ("st_lineinterpolatepointspheroid", 2),
    ("st_lineinterpolatepointssphere", 2),
    ("st_lineinterpolatepointsspheroid", 2),
    ("st_linesubstringsphere", 3),
    ("st_linesubstringspheroid", 3),
    ("st_azimuth", 2),
    ("st_project", 3),
    ("st_closestpoint", 2),
    // -- Set operations --
    ("st_union", 2),
    ("st_intersection", 2),
    ("st_difference", 2),
    ("st_symdifference", 2),
    ("st_buffer", 2),
    // -- Predicates --
    ("st_intersects", 2),
    ("st_contains", 2),
    ("st_within", 2),
    ("st_disjoint", 2),
    ("st_dwithin", 3),
    ("st_dwithinsphere", 3),
    ("st_dwithinspheroid", 3),
    ("st_covers", 2),
    ("st_coveredby", 2),
    ("st_equals", 2),
    ("st_touches", 2),
    ("st_crosses", 2),
    ("st_overlaps", 2),
    ("st_relate", 2),
    ("st_relate", 3),
    ("st_relatematch", 2),
    // -- Direct-only DDL helpers (SQLite-side) --
    ("createspatialindex", 2),
    ("dropspatialindex", 2),
];

/// Returns `true` when `(name, arity)` is implemented by SQLiteGIS. `name`
/// is matched case-insensitively against the lowercase catalog entries.
#[must_use]
pub fn is_sqlitegis_function(name: &str, arity: i32) -> bool {
    let lower = name.to_ascii_lowercase();
    POSTGIS_FUNCTION_CATALOG.iter().any(|&(n, a)| n == lower && a == arity)
}

/// Returns the set of arities SQLiteGIS implements for `name`, lowercased
/// case-insensitively. Empty when the function is unknown.
#[must_use]
pub fn sqlitegis_function_arities(name: &str) -> Vec<i32> {
    let lower = name.to_ascii_lowercase();
    POSTGIS_FUNCTION_CATALOG.iter().filter_map(|&(n, a)| (n == lower).then_some(a)).collect()
}

/// Returns `true` if `name` is shaped like a PostGIS scalar, used to flag
/// calls that look spatial but are absent from SQLiteGIS's catalog.
#[must_use]
pub fn is_postgis_shaped_name(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("st_")
}

// Shared between DDL translation (create_index.rs::try_spatial_index_routing,
// which emits `SELECT CreateSpatialIndex(...)` statements) and the pre-walk in
// Pg2Sqlite::translate_internal (which populates the spatial-index catalog so
// query-time predicate rewriting can consult it).

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{CreateIndex, Expr};

use crate::{
    errors::Error,
    impls::object_name::{last_ident, table_with_implicit_public_lookup},
};

/// Classifies a GiST `CreateIndex` against the schema.
///
/// - `Ok(Some(cols))` when every indexed entry is a bare column ref resolving
///   to a `geometry`/`geography` column. The vector preserves source order and
///   holds the column names exactly as written in the index.
/// - `Ok(None)` when no indexed entry classifies as spatial (the FTS5 path
///   handles those cases).
/// - `Err` when the index mixes spatial and non-spatial columns (the rewrite
///   semantics break), or when a `WHERE` partial-index predicate is set
///   (SQLiteGIS's `CreateSpatialIndex` rebuilds the rtree from every row).
///
/// The caller is expected to have already gated on
/// `create_index.using == Some(IndexType::GiST)`.
pub(crate) fn classify_gist_spatial_columns(
    create_index: &CreateIndex,
    schema: &ParserDB,
) -> Result<Option<Vec<String>>, Error> {
    let Some(table) = table_with_implicit_public_lookup(schema, &create_index.table_name)? else {
        return Ok(None);
    };

    let mut spatial_columns: Vec<String> = Vec::new();
    let mut non_spatial_columns: Vec<String> = Vec::new();
    for index_col in &create_index.columns {
        let Some(column_name) = simple_column_name(&index_col.column.expr) else {
            non_spatial_columns.push(format!("{}", index_col.column.expr));
            continue;
        };
        match table.column(&column_name, schema)?.map(|col| col.data_type(schema)) {
            Some(dt) if is_spatial_data_type(&dt) => spatial_columns.push(column_name),
            _ => non_spatial_columns.push(column_name),
        }
    }

    if spatial_columns.is_empty() {
        return Ok(None);
    }
    if !non_spatial_columns.is_empty() {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "GiST index on {} mixes spatial columns ({}) with non-spatial entries ({}); \
             SQLiteGIS's CreateSpatialIndex operates on a single geometry/geography column at a time.",
            create_index.table_name,
            spatial_columns.join(", "),
            non_spatial_columns.join(", ")
        )));
    }
    if create_index.predicate.is_some() {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "GiST partial-index `WHERE` predicate is not supported on spatial columns; \
             SQLiteGIS's CreateSpatialIndex rebuilds the rtree from every row of {}.",
            create_index.table_name
        )));
    }

    Ok(Some(spatial_columns))
}

/// Returns the column name if `expr` is a bare or compound identifier.
/// Anything else (functions, casts, binary ops) signals a non-trivial index
/// expression that the spatial routing can't safely classify.
#[must_use]
pub(crate) fn simple_column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| i.value.clone()),
        _ => None,
    }
}

/// Case-insensitive check for PostGIS-equivalent column types.
#[must_use]
pub(crate) fn is_spatial_data_type(dt: &str) -> bool {
    let lower = dt.trim().to_ascii_lowercase();
    lower == "geometry" || lower == "geography"
}

/// The SQLiteGIS name a measurement takes when its operand is a `geography`
/// column.
///
/// PostgreSQL measures `geometry` in the plane and `geography` on the WGS84
/// ellipsoid, and the translator used to send both to the planar
/// implementation, so a one-degree diagonal answered 1.41 where PostgreSQL
/// answers 156899.57 metres.
///
/// `None` means the call is not a measurement, or its operand is not resolvably
/// `geography`, and it keeps the spelling it came with. An operand the schema
/// does not settle is left planar rather than guessed at, because the two
/// readings differ in unit as well as magnitude.
pub(crate) fn geography_measure_name(
    name: &str,
    first_argument: Option<&Expr>,
    schema: &ParserDB,
) -> Option<&'static str> {
    const MEASURES: &[(&str, &str)] = &[
        ("st_distance", "ST_DistanceSpheroid"),
        ("st_dwithin", "ST_DWithinSpheroid"),
        ("st_length", "ST_LengthSpheroid"),
        ("st_area", "ST_AreaSpheroid"),
        ("st_perimeter", "ST_PerimeterSpheroid"),
    ];
    let lower = name.to_ascii_lowercase();
    let (_, routed) = MEASURES.iter().find(|(measure, _)| *measure == lower)?;
    let operand = first_argument?;
    every_declared_type_matches(operand, schema, |declared| {
        declared.trim().eq_ignore_ascii_case("geography")
    })
    .then_some(*routed)
}

// When the input contains a `SELECT ... WHERE ST_*(col, expr) ...` over a
// column whose table has a translated spatial index, the rewrite injects a
// `WHERE <table>.rowid IN (SELECT id FROM <rtree> WHERE bbox-conditions)`
// pre-filter so SQLite's planner reaches the rtree virtual table without the
// user having to write the JOIN by hand.
//
// Conservative v1 scope: single-table FROM, flat top-level AND in WHERE,
// bbox-overlap-narrowable predicate over a bare or correctly-qualified column
// reference. Anything else passes through unchanged.

use sqlparser::{
    ast::{BinaryOperator, Select, TableFactor, TableWithJoins},
    dialect::PostgreSqlDialect,
};

use crate::{
    impls::{
        generated_sql::parse_single_generated_sql,
        shared_helpers::{every_declared_type_matches, function_argument_exprs},
    },
    options::Pg2SqliteOptions,
};

/// Bbox-overlap-narrowable spatial predicates. For each of these, a positive
/// answer implies the two geometries' bounding boxes overlap, so an rtree
/// pre-filter on the indexed column's bbox is a lossless candidate-set
/// reducer. `ST_Disjoint` is intentionally absent: its truth condition is the
/// opposite (no overlap), so an rtree-overlap pre-filter would discard rows
/// the predicate would otherwise accept.
#[must_use]
pub(crate) fn is_bbox_narrowable_predicate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "st_intersects"
            | "st_contains"
            | "st_within"
            | "st_covers"
            | "st_coveredby"
            | "st_equals"
            | "st_touches"
            | "st_crosses"
            | "st_overlaps"
    )
}

/// Returns a rewritten `Select` when `select`'s shape is eligible for spatial
/// predicate rewriting against the spatial index catalog in `options`, or
/// `Ok(None)` to fall through to the original.
pub(crate) fn try_rewrite_spatial_select(
    select: &Select,
    options: &Pg2SqliteOptions,
) -> Result<Option<Select>, crate::errors::Error> {
    if select.from.len() != 1 {
        return Ok(None);
    }
    let Some(new_selection) =
        compute_rtree_filtered_where(&select.from[0], select.selection.as_ref(), options)?
    else {
        return Ok(None);
    };
    let mut rewritten = select.clone();
    rewritten.selection = Some(new_selection);
    Ok(Some(rewritten))
}

/// Shared analysis half of the spatial rewrite, used by SELECT, UPDATE, and
/// DELETE wrappers. Resolves the single-table base and the WHERE clause,
/// then asks `try_build_rtree_filter` to produce the rewritten predicate.
/// Returns `Ok(None)` for any shape the rewrite can't handle so callers
/// fall through to their existing translation.
fn compute_rtree_filtered_where(
    base_twj: &TableWithJoins,
    where_expr: Option<&sqlparser::ast::Expr>,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Expr>, crate::errors::Error> {
    let Some((base_table_name, base_alias)) = single_base_table(base_twj) else {
        return Ok(None);
    };
    let Some(where_expr) = where_expr else {
        return Ok(None);
    };
    try_build_rtree_filter(&base_table_name, base_alias.as_deref(), where_expr, options)
}

/// Given a single-table base reference plus its WHERE clause, returns a new
/// WHERE expression that pre-filters via the rtree shadow, or `Ok(None)` when
/// the shape isn't eligible. Shared by the SELECT, UPDATE, and DELETE rewrite
/// paths so the analysis and SQL synthesis live in one place.
///
/// The returned expression has shape `(<base>.rowid IN (SELECT id FROM <rtree>
/// WHERE bbox-conditions)) AND (<original_where>)`, parsed from a synthesized
/// `SELECT 1 WHERE ...` so we can reuse the existing PG-dialect parser instead
/// of constructing the AST by hand.
pub(crate) fn try_build_rtree_filter(
    base_table_name: &str,
    base_alias: Option<&str>,
    where_expr: &sqlparser::ast::Expr,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Expr>, crate::errors::Error> {
    if !is_safe_top_level_and(where_expr) {
        return Ok(None);
    }
    let conjuncts = flatten_top_level_and(where_expr);

    let base_table_lower = base_table_name.to_ascii_lowercase();
    let base_alias_lower = base_alias.map(str::to_ascii_lowercase);

    let spatial = conjuncts.iter().find_map(|c| {
        extract_spatial_filter(c, &base_table_lower, base_alias_lower.as_deref(), options)
    });
    let Some(spatial) = spatial else {
        return Ok(None);
    };

    let rtree_table = format!("{}_{}_rtree", base_table_lower, spatial.column);
    let base_for_rowid = base_alias.map_or_else(|| base_table_name.to_string(), str::to_string);
    let geom_sql = format!("{}", spatial.geom_expr);
    let original_where = format!("{where_expr}");

    let probe_sql = format!(
        "SELECT 1 WHERE \
         ({base_for_rowid}.rowid IN ( \
             SELECT id FROM {rtree_table} \
             WHERE xmin <= ST_XMax({geom_sql}) AND xmax >= ST_XMin({geom_sql}) \
               AND ymin <= ST_YMax({geom_sql}) AND ymax >= ST_YMin({geom_sql}) \
         )) AND ({original_where})"
    );

    let stmt = parse_single_generated_sql(
        &PostgreSqlDialect {},
        &probe_sql,
        "spatial predicate rewriting",
    )?;
    let sqlparser::ast::Statement::Query(query) = stmt else {
        return Ok(None);
    };
    let sqlparser::ast::SetExpr::Select(inner) = *query.body else {
        return Ok(None);
    };
    Ok(inner.selection)
}

/// Returns a rewritten `Delete` when the target is a single base table
/// without `USING` and the WHERE clause carries a rewriteable spatial
/// predicate. Otherwise returns `Ok(None)`. `DELETE ... USING` is naturally
/// excluded: the `<Delete as Translator>::translate` caller wraps its WHERE
/// in `EXISTS(subquery)` before invoking this helper, and that shape fails
/// the flat-AND check inside `try_build_rtree_filter`.
pub(crate) fn try_rewrite_spatial_delete(
    delete: &sqlparser::ast::Delete,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Delete>, crate::errors::Error> {
    use sqlparser::ast::FromTable;

    if delete.using.as_ref().is_some_and(|u| !u.is_empty()) {
        return Ok(None);
    }
    let from_tables = match &delete.from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    };
    if from_tables.len() != 1 {
        return Ok(None);
    }
    let Some(new_selection) =
        compute_rtree_filtered_where(&from_tables[0], delete.selection.as_ref(), options)?
    else {
        return Ok(None);
    };
    let mut rewritten = delete.clone();
    rewritten.selection = Some(new_selection);
    Ok(Some(rewritten))
}

/// Returns a rewritten `Update` when the target's shape is a single base
/// table without an `UPDATE ... FROM` extension and the WHERE clause carries
/// a rewriteable spatial predicate. Otherwise returns `Ok(None)`.
pub(crate) fn try_rewrite_spatial_update(
    update: &sqlparser::ast::Update,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::Update>, crate::errors::Error> {
    // UPDATE ... FROM is multi-source; out of scope for v1.
    if update.from.is_some() {
        return Ok(None);
    }
    let Some(new_selection) =
        compute_rtree_filtered_where(&update.table, update.selection.as_ref(), options)?
    else {
        return Ok(None);
    };
    let mut rewritten = update.clone();
    rewritten.selection = Some(new_selection);
    Ok(Some(rewritten))
}

/// Returns `(table_name, alias)` from a `TableWithJoins` that names exactly
/// one base table, with no joins, no table-valued-function args, and no
/// subquery / derived shape. Used by SELECT (via `select.from[0]`), UPDATE
/// (via `update.table`), and DELETE (via `delete.from[0]`).
pub(crate) fn single_base_table(twj: &TableWithJoins) -> Option<(String, Option<String>)> {
    if !twj.joins.is_empty() {
        return None;
    }
    let TableFactor::Table { name, alias, args: None, .. } = &twj.relation else {
        return None;
    };
    let table = last_ident(name).map(|i| i.value.clone())?;
    let alias_name = alias.as_ref().map(|a| a.name.value.clone());
    Some((table, alias_name))
}

/// Returns `true` when `expr` is composed only of top-level AND conjunctions
/// (no `OR`, no `NOT`). Conjuncts may themselves contain `OR` or `NOT`
/// internally - what we forbid is `OR` or `NOT` at the boolean root, because
/// that would mean the spatial predicate isn't a required filter.
fn is_safe_top_level_and(expr: &sqlparser::ast::Expr) -> bool {
    match expr {
        sqlparser::ast::Expr::BinaryOp { op: BinaryOperator::And, left, right } => {
            is_safe_top_level_and(left) && is_safe_top_level_and(right)
        }
        sqlparser::ast::Expr::BinaryOp { op: BinaryOperator::Or, .. }
        | sqlparser::ast::Expr::UnaryOp { op: sqlparser::ast::UnaryOperator::Not, .. } => false,
        _ => true,
    }
}

/// Splits a flat AND chain into its top-level conjuncts. Caller must ensure
/// `expr` is `OR`/`NOT`-free at the root via [`is_safe_top_level_and`].
fn flatten_top_level_and(expr: &sqlparser::ast::Expr) -> Vec<&sqlparser::ast::Expr> {
    let mut out = Vec::new();
    walk_and(expr, &mut out);
    out
}

fn walk_and<'a>(expr: &'a sqlparser::ast::Expr, out: &mut Vec<&'a sqlparser::ast::Expr>) {
    if let sqlparser::ast::Expr::BinaryOp { op: BinaryOperator::And, left, right } = expr {
        walk_and(left, out);
        walk_and(right, out);
    } else {
        out.push(expr);
    }
}

pub(crate) struct SpatialFilter<'a> {
    pub column: String,
    pub geom_expr: &'a sqlparser::ast::Expr,
}

/// Classifies a conjunct as a rewriteable spatial filter against the
/// (base_table, *) entries in the spatial-index catalog. Returns `None` if
/// the conjunct isn't a bbox-narrowable spatial function call, if its first
/// arg isn't a column ref resolving to the base table, or if the resulting
/// (table, column) pair isn't in the catalog.
fn extract_spatial_filter<'a>(
    expr: &'a sqlparser::ast::Expr,
    base_table_lower: &str,
    base_alias_lower: Option<&str>,
    options: &Pg2SqliteOptions,
) -> Option<SpatialFilter<'a>> {
    let sqlparser::ast::Expr::Function(func) = expr else {
        return None;
    };
    let name = last_ident(&func.name).map(|i| i.value.clone())?;
    if !is_bbox_narrowable_predicate(&name) {
        return None;
    }
    let args = function_argument_exprs(&func.args);
    if args.len() < 2 {
        return None;
    }
    let column = column_resolving_to_base(args[0], base_table_lower, base_alias_lower)?;
    if !options.has_spatial_index(base_table_lower, &column) {
        return None;
    }
    Some(SpatialFilter { column, geom_expr: args[1] })
}

/// Returns the column name when `arg` is a bare identifier or a two-part
/// compound identifier whose qualifier matches the base table name or its
/// FROM-list alias. Rejects deeper compound forms and non-identifier args
/// so we don't rewrite when the predicate's first operand is an expression
/// (e.g. `ST_Buffer(geom, 1.0)`) or a column from an unrelated table.
fn column_resolving_to_base(
    arg: &sqlparser::ast::Expr,
    base_table_lower: &str,
    base_alias_lower: Option<&str>,
) -> Option<String> {
    match arg {
        sqlparser::ast::Expr::Identifier(ident) => Some(ident.value.clone()),
        sqlparser::ast::Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
            let qualifier = parts[0].value.to_ascii_lowercase();
            let column = parts[1].value.clone();
            let qualifier_ok =
                qualifier == base_table_lower || base_alias_lower.is_some_and(|a| a == qualifier);
            qualifier_ok.then_some(column)
        }
        _ => None,
    }
}
