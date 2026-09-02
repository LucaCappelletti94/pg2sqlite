//! Step 4 (sqlitegis-gated): the translated GiST->CreateSpatialIndex DDL
//! executes against a real SQLiteGIS-loaded SQLite, producing the rtree
//! shadow table at `<table>_<col>_rtree`, and subsequent spatial queries
//! work against the indexed column.
//!
//! Run with: `cargo test --features sqlitegis --test test_postgis_gist_diesel`

#![cfg(feature = "sqlitegis")]

mod helpers;

use diesel::{
    QueryableByName, RunQueryDsl, SqliteConnection, sql_query,
    sql_types::{Integer, Text},
};
use helpers::sqlitegis::sqlitegis_connection;
use pg2sqlite::prelude::Pg2SqliteOptions;

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = Integer)]
    n: i32,
}

#[derive(QueryableByName)]
struct NameRow {
    #[diesel(sql_type = Text)]
    name: String,
}

#[derive(QueryableByName)]
struct PlanRow {
    #[diesel(sql_type = Text)]
    detail: String,
}

/// Runs `EXPLAIN QUERY PLAN <sql>` and returns the planner's `detail` column
/// rows joined with newlines. Tests assert substrings (such as
/// `"VIRTUAL TABLE INDEX"`) on the returned string.
fn explain_plan_text(conn: &mut SqliteConnection, sql: &str) -> String {
    let plan: Vec<PlanRow> = sql_query(format!("EXPLAIN QUERY PLAN {sql}"))
        .load(conn)
        .unwrap_or_else(|e| panic!("EXPLAIN QUERY PLAN for `{sql}`: {e:?}"));
    plan.iter().map(|p| p.detail.as_str()).collect::<Vec<_>>().join("\n")
}

/// Inserts a 100x100 grid of `ST_Point(i, j)` rows into `table`. The caller
/// manages the transaction (with `BEGIN` / `COMMIT`) so paired tests can
/// populate two tables atomically inside a single transaction.
fn populate_grid_points(conn: &mut SqliteConnection, table: &str) {
    for i in 0..100 {
        for j in 0..100 {
            let id = i * 100 + j;
            sql_query(format!("INSERT INTO {table} (id, geom) VALUES ({id}, ST_Point({i}, {j}))"))
                .execute(conn)
                .unwrap_or_else(|e| panic!("insert into {table}: {e:?}"));
        }
    }
}

#[test]
fn gist_geometry_index_round_trips_via_diesel() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    let pg_sql = "CREATE TABLE features (id INTEGER PRIMARY KEY, geom geometry); \
                  CREATE INDEX features_geom_idx ON features USING gist (geom);";
    let stmts = helpers::translate_pg(pg_sql, &opts).expect("translate");

    for s in &stmts {
        sql_query(s)
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("execute failed for `{s}`: {e:?}"));
    }

    // SQLiteGIS names the rtree shadow as `<table>_<col>_rtree`.
    let rows: Vec<NameRow> = sql_query(
        "SELECT name FROM sqlite_master WHERE name = 'features_geom_rtree' AND type = 'table'",
    )
    .load(&mut conn)
    .expect("query sqlite_master");
    assert_eq!(rows.len(), 1, "rtree shadow table should exist after CreateSpatialIndex");
    assert_eq!(rows[0].name, "features_geom_rtree");

    // Insert one row and verify the spatial predicate finds it. The AFTER
    // INSERT trigger installed by SQLiteGIS's CreateSpatialIndex also
    // populates the rtree shadow, but this test focuses on translation
    // shape only; the full-grid acceleration test below covers rtree sync
    // and planner usage explicitly.
    sql_query("INSERT INTO features (id, geom) VALUES (1, ST_Point(0.5, 0.5))")
        .execute(&mut conn)
        .expect("insert point");

    let rows: Vec<CountRow> = sql_query(
        "SELECT count(*) AS n FROM features \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(0.0, 0.0, 1.0, 1.0))",
    )
    .load(&mut conn)
    .expect("spatial query");
    assert_eq!(rows[0].n, 1, "ST_Intersects should match the inserted point");
}

/// Verifies that pg2sqlite's translation of `CREATE INDEX ... USING gist
/// (geom)` produces a maintained, query-accelerating rtree shadow when the
/// SQLiteGIS extension is loaded:
///
/// 1. After the translated DDL runs, the rtree shadow exists and is empty.
/// 2. AFTER INSERT triggers installed by SQLiteGIS's `CreateSpatialIndex`
///    populate the rtree as rows are written, with no manual sync.
/// 3. A bounding-box probe of the rtree alone narrows the candidate set to a
///    tiny fraction of the base table, demonstrating that the index is
///    mechanically functional (the planner won't auto-join through it on a
///    plain `ST_Intersects` predicate, but explicit joins are fast).
/// 4. The rtree-join query and the full-scan query return the same count,
///    proving correctness of the indexed candidate set.
/// 5. `EXPLAIN QUERY PLAN` on the rtree-join confirms the planner reaches the
///    rtree virtual table.
#[test]
fn gist_geometry_index_accelerates_spatial_queries() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    // 1. Translate and execute the PG-shaped DDL.
    let pg_sql = "CREATE TABLE perf_grid (id INTEGER PRIMARY KEY, geom geometry); \
                  CREATE INDEX perf_grid_geom_idx ON perf_grid USING gist (geom);";
    let stmts = helpers::translate_pg(pg_sql, &opts).expect("translate");
    for s in &stmts {
        sql_query(s)
            .execute(&mut conn)
            .unwrap_or_else(|e| panic!("execute failed for `{s}`: {e:?}"));
    }

    // 2. Rtree shadow exists and is empty before any writes.
    let rtree_initial: Vec<CountRow> = sql_query("SELECT count(*) AS n FROM perf_grid_geom_rtree")
        .load(&mut conn)
        .expect("rtree count (initial)");
    assert_eq!(rtree_initial[0].n, 0, "rtree must start empty on a freshly-translated GiST index");

    // 3. Insert 10k points on a 100x100 grid. The AFTER INSERT triggers
    //    installed by SQLiteGIS's CreateSpatialIndex must populate the rtree
    //    transparently; pg2sqlite emits no extra sync SQL of its own.
    sql_query("BEGIN").execute(&mut conn).expect("begin tx");
    populate_grid_points(&mut conn, "perf_grid");
    sql_query("COMMIT").execute(&mut conn).expect("commit tx");

    let base_count: Vec<CountRow> =
        sql_query("SELECT count(*) AS n FROM perf_grid").load(&mut conn).expect("base count");
    assert_eq!(base_count[0].n, 10_000);

    let rtree_after: Vec<CountRow> = sql_query("SELECT count(*) AS n FROM perf_grid_geom_rtree")
        .load(&mut conn)
        .expect("rtree count (after writes)");
    assert_eq!(
        rtree_after[0].n, 10_000,
        "SQLiteGIS triggers must keep the rtree synchronized with inserts; \
         got {} entries against {} base rows",
        rtree_after[0].n, base_count[0].n
    );

    // 4. Rtree-only bbox probe narrows the candidate set deterministically. The
    //    (20..=30, 20..=30) window covers an 11x11 grid: 121 points.
    let candidates: Vec<CountRow> = sql_query(
        "SELECT count(*) AS n FROM perf_grid_geom_rtree \
         WHERE xmin >= 20 AND xmax <= 30 AND ymin >= 20 AND ymax <= 30",
    )
    .load(&mut conn)
    .expect("rtree bbox probe");
    assert_eq!(candidates[0].n, 121, "rtree bbox probe must isolate the 11x11 window");
    assert!(
        candidates[0].n * 50 < base_count[0].n,
        "rtree must narrow candidates by at least 50x (got {} of {})",
        candidates[0].n,
        base_count[0].n
    );

    // 5. Full-scan and rtree-join must agree on the answer count.
    let full_scan: Vec<CountRow> = sql_query(
        "SELECT count(*) AS n FROM perf_grid \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20.0, 20.0, 30.0, 30.0)) = 1",
    )
    .load(&mut conn)
    .expect("full-scan ST_Intersects");
    let rtree_join: Vec<CountRow> = sql_query(
        "SELECT count(*) AS n FROM perf_grid p \
         JOIN perf_grid_geom_rtree r ON p.id = r.id \
         WHERE r.xmin >= 20 AND r.xmax <= 30 AND r.ymin >= 20 AND r.ymax <= 30 \
           AND ST_Intersects(p.geom, ST_MakeEnvelope(20.0, 20.0, 30.0, 30.0)) = 1",
    )
    .load(&mut conn)
    .expect("rtree-join ST_Intersects");
    assert_eq!(
        full_scan[0].n, 121,
        "ST_Intersects full scan must return all 121 points in the window"
    );
    assert_eq!(
        rtree_join[0].n, full_scan[0].n,
        "rtree-join query must return the same count as the full scan"
    );

    // 6. Query plan: the rtree-join must reach the rtree virtual table.
    //    SQLite's EXPLAIN QUERY PLAN reports `VIRTUAL TABLE INDEX <n>:<id>` for
    //    rtree scans (and only for rtree-style virtual tables in this schema),
    //    so the presence of that marker is sufficient evidence that the planner
    //    is using the index rather than falling back to a table scan on
    //    `perf_grid_geom_rtree`'s shadow rows.
    let plan_text = explain_plan_text(
        &mut conn,
        "SELECT p.id FROM perf_grid p \
         JOIN perf_grid_geom_rtree r ON p.id = r.id \
         WHERE r.xmin >= 20 AND r.xmax <= 30 AND r.ymin >= 20 AND r.ymax <= 30",
    );
    assert!(
        plan_text.contains("VIRTUAL TABLE INDEX"),
        "query plan should drive the rtree virtual-table index; got:\n{plan_text}"
    );
}

/// Step 6 (predicate rewriting): a plain `SELECT ... WHERE ST_Intersects(geom,
/// envelope)` over an indexed column must, after translation, hit the rtree
/// without the user writing the JOIN explicitly.
#[test]
fn st_intersects_on_indexed_column_uses_rtree_via_translation() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    // Translate the schema together with the user's spatial query so that
    // pg2sqlite's spatial-index catalog sees the GiST DDL before reaching
    // the SELECT. This is how a real consumer drives pg2sqlite: feed it the
    // full schema and the queries that target it, in one call.
    let full_sql = "CREATE TABLE perf_grid (id INTEGER PRIMARY KEY, geom geometry); \
                    CREATE INDEX perf_grid_geom_idx ON perf_grid USING gist (geom); \
                    SELECT id FROM perf_grid \
                     WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30));";
    let translated = helpers::translate_pg(full_sql, &opts).expect("translate schema + query");

    // Execute the schema-shaped statements (CREATE TABLE, CreateSpatialIndex).
    // The user SELECT comes last; we run it separately so we can inspect its
    // query plan in isolation.
    let user_select = helpers::user_statement_of(&translated, "SELECT");
    for s in &translated {
        if std::ptr::eq(s, user_select) {
            continue;
        }
        sql_query(s).execute(&mut conn).expect("execute schema stmt");
    }

    sql_query("BEGIN").execute(&mut conn).expect("begin tx");
    populate_grid_points(&mut conn, "perf_grid");
    sql_query("COMMIT").execute(&mut conn).expect("commit");

    let select = user_select;

    // Correctness: same answer count as the unindexed full scan would give.
    let count: Vec<CountRow> = sql_query(format!("SELECT count(*) AS n FROM ({select})"))
        .load(&mut conn)
        .expect("execute translated SELECT");
    assert_eq!(count[0].n, 121, "rewritten query must return 11x11 = 121 points");

    // Plan: the translated SELECT, run as-is by the user, must reach the
    // rtree virtual table without any manual JOIN.
    let plan_text = explain_plan_text(&mut conn, select);
    assert!(
        plan_text.contains("VIRTUAL TABLE INDEX"),
        "translated SELECT should hit the rtree virtual table; got plan:\n{plan_text}\n\
         translated SQL was:\n{select}"
    );
}

/// Proves the acceleration is conditional on index presence by running the
/// SAME spatial predicate against two identically-populated tables - one
/// with a translated GiST index, one without - and asserting the query
/// plans differ accordingly while both return the same correct row count.
#[test]
fn acceleration_is_conditional_on_index_presence() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    // Both tables share the same shape; only the indexed one gets a
    // CREATE INDEX USING gist. The two SELECTs are byte-identical apart
    // from their FROM target, so any plan or output difference must come
    // from the index translation path.
    let full_sql = "\
        CREATE TABLE indexed_grid (id INTEGER PRIMARY KEY, geom geometry); \
        CREATE TABLE unindexed_grid (id INTEGER PRIMARY KEY, geom geometry); \
        CREATE INDEX indexed_grid_geom_idx ON indexed_grid USING gist (geom); \
        SELECT id FROM indexed_grid \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30)); \
        SELECT id FROM unindexed_grid \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30));";

    let translated = helpers::translate_pg(full_sql, &opts).expect("translate");

    // Pick out the two user SELECTs (skip CREATE TABLE + CreateSpatialIndex).
    let selects: Vec<&String> =
        translated.iter().filter(|s| helpers::is_user_statement(s, "SELECT")).collect();
    assert_eq!(selects.len(), 2, "expected two translated user SELECTs, got: {selects:?}");
    let indexed_select =
        selects.iter().find(|s| s.contains("indexed_grid")).expect("indexed SELECT").as_str();
    let unindexed_select = selects
        .iter()
        .find(|s| s.contains("unindexed_grid") && !s.contains("indexed_grid_geom_rtree"))
        .expect("unindexed SELECT")
        .as_str();

    // Translation-side assertion: only the indexed query carries the rewrite.
    assert!(
        indexed_select.contains("indexed_grid_geom_rtree"),
        "indexed query must be rewritten via the rtree shadow, got:\n{indexed_select}"
    );
    assert!(
        !unindexed_select.contains("_rtree"),
        "unindexed query must not reference any rtree shadow, got:\n{unindexed_select}"
    );

    // Apply the schema-shaped statements; skip the SELECTs (we run those
    // separately so we can inspect their query plans).
    for s in &translated {
        if helpers::is_user_statement(s, "SELECT") {
            continue;
        }
        sql_query(s).execute(&mut conn).expect("execute schema stmt");
    }

    // Populate both tables identically with the same 10k-point grid so the
    // ST_Intersects answers are guaranteed equal.
    sql_query("BEGIN").execute(&mut conn).expect("begin tx");
    populate_grid_points(&mut conn, "indexed_grid");
    populate_grid_points(&mut conn, "unindexed_grid");
    sql_query("COMMIT").execute(&mut conn).expect("commit");

    // Correctness: both queries return the same row count.
    let indexed_count: Vec<CountRow> =
        sql_query(format!("SELECT count(*) AS n FROM ({indexed_select})"))
            .load(&mut conn)
            .expect("count indexed");
    let unindexed_count: Vec<CountRow> =
        sql_query(format!("SELECT count(*) AS n FROM ({unindexed_select})"))
            .load(&mut conn)
            .expect("count unindexed");
    assert_eq!(indexed_count[0].n, 121, "indexed query must find 11x11 = 121 points");
    assert_eq!(
        unindexed_count[0].n, indexed_count[0].n,
        "both queries must return the same count over identical data"
    );

    // Plan: indexed reaches the rtree virtual table; unindexed full-scans.
    let indexed_text = explain_plan_text(&mut conn, indexed_select);
    let unindexed_text = explain_plan_text(&mut conn, unindexed_select);

    assert!(
        indexed_text.contains("VIRTUAL TABLE INDEX"),
        "indexed plan must drive the rtree; got:\n{indexed_text}\nfor SQL:\n{indexed_select}"
    );
    assert!(
        !unindexed_text.contains("VIRTUAL TABLE INDEX"),
        "unindexed plan must NOT mention the rtree (would mean leakage); got:\n{unindexed_text}\n\
         for SQL:\n{unindexed_select}"
    );
    assert!(
        unindexed_text.contains("SCAN"),
        "unindexed plan should be a full table scan; got:\n{unindexed_text}"
    );
}

/// Like `acceleration_is_conditional_on_index_presence` but for `UPDATE`:
/// runs the same spatial UPDATE against two identically-populated tables
/// (one indexed, one not) and asserts the plan reaches the rtree only for
/// the indexed table while both updates touch the same row set.
#[test]
fn update_acceleration_is_conditional_on_index_presence() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    let full_sql = "\
        CREATE TABLE indexed_grid (id INTEGER PRIMARY KEY, geom geometry, marker text); \
        CREATE TABLE unindexed_grid (id INTEGER PRIMARY KEY, geom geometry, marker text); \
        CREATE INDEX indexed_grid_geom_idx ON indexed_grid USING gist (geom); \
        UPDATE indexed_grid SET marker = 'hit' \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30)); \
        UPDATE unindexed_grid SET marker = 'hit' \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30));";
    let translated = helpers::translate_pg(full_sql, &opts).expect("translate");

    let updates: Vec<&String> =
        translated.iter().filter(|s| helpers::is_user_statement(s, "UPDATE")).collect();
    assert_eq!(updates.len(), 2, "expected two translated UPDATEs, got: {updates:?}");
    let indexed_update = updates
        .iter()
        .find(|s| s.contains("indexed_grid") && !s.contains("unindexed_grid"))
        .expect("indexed UPDATE")
        .as_str();
    let unindexed_update =
        updates.iter().find(|s| s.contains("unindexed_grid")).expect("unindexed UPDATE").as_str();
    assert!(
        indexed_update.contains("indexed_grid_geom_rtree"),
        "indexed UPDATE must be rewritten via the rtree shadow, got:\n{indexed_update}"
    );
    assert!(
        !unindexed_update.contains("_rtree"),
        "unindexed UPDATE must not reference any rtree shadow, got:\n{unindexed_update}"
    );

    // Apply schema + CreateSpatialIndex; skip the UPDATEs.
    for s in &translated {
        if helpers::is_user_statement(s, "UPDATE") {
            continue;
        }
        sql_query(s).execute(&mut conn).expect("execute schema stmt");
    }

    sql_query("BEGIN").execute(&mut conn).expect("begin tx");
    populate_grid_points(&mut conn, "indexed_grid");
    populate_grid_points(&mut conn, "unindexed_grid");
    sql_query("COMMIT").execute(&mut conn).expect("commit");

    // EXPLAIN QUERY PLAN before running, so we can observe the planner's
    // choice without depending on per-row execution semantics.
    let indexed_text = explain_plan_text(&mut conn, indexed_update);
    let unindexed_text = explain_plan_text(&mut conn, unindexed_update);
    assert!(
        indexed_text.contains("VIRTUAL TABLE INDEX"),
        "indexed UPDATE plan must drive the rtree; got:\n{indexed_text}\nfor SQL:\n{indexed_update}"
    );
    assert!(
        !unindexed_text.contains("VIRTUAL TABLE INDEX"),
        "unindexed UPDATE plan must NOT mention the rtree (would mean leakage); got:\n{unindexed_text}"
    );

    // Run both UPDATEs and assert each marked exactly the 11x11 grid window.
    sql_query(indexed_update).execute(&mut conn).expect("execute indexed UPDATE");
    sql_query(unindexed_update).execute(&mut conn).expect("execute unindexed UPDATE");
    let indexed_hits: Vec<CountRow> =
        sql_query("SELECT count(*) AS n FROM indexed_grid WHERE marker = 'hit'")
            .load(&mut conn)
            .expect("count indexed hits");
    let unindexed_hits: Vec<CountRow> =
        sql_query("SELECT count(*) AS n FROM unindexed_grid WHERE marker = 'hit'")
            .load(&mut conn)
            .expect("count unindexed hits");
    assert_eq!(indexed_hits[0].n, 121, "indexed UPDATE must mark 11x11 = 121 rows");
    assert_eq!(
        unindexed_hits[0].n, indexed_hits[0].n,
        "both UPDATEs must touch the same row set over identical data"
    );
}

/// Like the above but for `DELETE`: same spatial predicate against two
/// identically-populated tables; the indexed one's plan reaches the rtree,
/// the unindexed one full-scans, both end up with the same surviving rows.
#[test]
fn delete_acceleration_is_conditional_on_index_presence() {
    let mut conn = sqlitegis_connection();
    let opts = Pg2SqliteOptions::default().with_sqlitegis_enabled();

    let full_sql = "\
        CREATE TABLE indexed_grid (id INTEGER PRIMARY KEY, geom geometry); \
        CREATE TABLE unindexed_grid (id INTEGER PRIMARY KEY, geom geometry); \
        CREATE INDEX indexed_grid_geom_idx ON indexed_grid USING gist (geom); \
        DELETE FROM indexed_grid \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30)); \
        DELETE FROM unindexed_grid \
         WHERE ST_Intersects(geom, ST_MakeEnvelope(20, 20, 30, 30));";
    let translated = helpers::translate_pg(full_sql, &opts).expect("translate");

    let deletes: Vec<&String> =
        translated.iter().filter(|s| helpers::is_user_statement(s, "DELETE")).collect();
    assert_eq!(deletes.len(), 2, "expected two translated DELETEs, got: {deletes:?}");
    let indexed_delete = deletes
        .iter()
        .find(|s| s.contains("indexed_grid") && !s.contains("unindexed_grid"))
        .expect("indexed DELETE")
        .as_str();
    let unindexed_delete =
        deletes.iter().find(|s| s.contains("unindexed_grid")).expect("unindexed DELETE").as_str();
    assert!(
        indexed_delete.contains("indexed_grid_geom_rtree"),
        "indexed DELETE must be rewritten via the rtree shadow, got:\n{indexed_delete}"
    );
    assert!(
        !unindexed_delete.contains("_rtree"),
        "unindexed DELETE must not reference any rtree shadow, got:\n{unindexed_delete}"
    );

    for s in &translated {
        if helpers::is_user_statement(s, "DELETE") {
            continue;
        }
        sql_query(s).execute(&mut conn).expect("execute schema stmt");
    }

    sql_query("BEGIN").execute(&mut conn).expect("begin tx");
    populate_grid_points(&mut conn, "indexed_grid");
    populate_grid_points(&mut conn, "unindexed_grid");
    sql_query("COMMIT").execute(&mut conn).expect("commit");

    let indexed_text = explain_plan_text(&mut conn, indexed_delete);
    let unindexed_text = explain_plan_text(&mut conn, unindexed_delete);
    assert!(
        indexed_text.contains("VIRTUAL TABLE INDEX"),
        "indexed DELETE plan must drive the rtree; got:\n{indexed_text}"
    );
    assert!(
        !unindexed_text.contains("VIRTUAL TABLE INDEX"),
        "unindexed DELETE plan must NOT mention the rtree; got:\n{unindexed_text}"
    );

    sql_query(indexed_delete).execute(&mut conn).expect("execute indexed DELETE");
    sql_query(unindexed_delete).execute(&mut conn).expect("execute unindexed DELETE");
    let indexed_remaining: Vec<CountRow> = sql_query("SELECT count(*) AS n FROM indexed_grid")
        .load(&mut conn)
        .expect("count indexed remaining");
    let unindexed_remaining: Vec<CountRow> = sql_query("SELECT count(*) AS n FROM unindexed_grid")
        .load(&mut conn)
        .expect("count unindexed remaining");
    assert_eq!(
        indexed_remaining[0].n, 9879,
        "indexed DELETE must remove 11x11 = 121 of the 10000 rows"
    );
    assert_eq!(
        unindexed_remaining[0].n, indexed_remaining[0].n,
        "both DELETEs must remove the same row set over identical data"
    );
}
