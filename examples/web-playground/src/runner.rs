//! Run user-typed SQL against the live Diesel connection and read
//! rows generically. We use `LoadConnection::load` to get a cursor of
//! dynamic `Row`s, then read each cell by inspecting its `SqliteType`
//! and calling the matching `SqliteValue::read_*` accessor.
//!
//! For statements that don't produce rows (CREATE, INSERT, UPDATE, ...)
//! Diesel's `load` still works - it just yields zero rows - and we
//! follow up with a `changes()` query to report the affected row
//! count. This is ported almost verbatim from
//! `geolite/examples/web-demo/src/runner.rs`; the only deletions are
//! the sqlitegis-specific `:lon`/`:lat` placeholder substitution and
//! the `extract_lonlat` helper. The latter moves to `geom.rs` in
//! Step 4 once we know whether the result row carried a geometry
//! BLOB.

use diesel::{
    RunQueryDsl,
    connection::{LoadConnection, SimpleConnection},
    deserialize::QueryableByName,
    row::{Field, Row},
    sql_types::BigInt,
    sqlite::{Sqlite, SqliteType},
};

use crate::db;

#[derive(Clone, Debug, PartialEq)]
pub struct QueryRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueryOutcome {
    /// A SELECT-style result with column headers and row data.
    Rows { result: QueryRows, elapsed_ms: f64 },
    /// A DDL / DML statement that produced no rows. `rows` is
    /// SQLite's `changes()` count for the last statement.
    Affected { rows: i64, elapsed_ms: f64 },
    /// Anything went wrong: parse error, constraint violation,
    /// missing function, ...
    Error(String),
}

/// Run a SQL script (one or many `;`-separated statements) against
/// the in-memory connection. If the script's last statement produced
/// rows, return them; otherwise return the affected-row count from
/// SQLite's `changes()`.
pub fn run(sql: &str) -> QueryOutcome {
    let start = performance_now();

    type RawCapture = Result<(Option<Vec<String>>, Vec<Vec<String>>), String>;

    let outcome = db::with_conn(|conn| -> Result<QueryOutcome, String> {
        // The cursor holds a `&mut` borrow on `conn`; drain it in a
        // nested scope and drop it before touching `conn` again for
        // the changes() fallback.
        let captured: RawCapture = {
            let cursor = LoadConnection::<diesel::connection::DefaultLoadingMode>::load(
                conn,
                diesel::sql_query(sql),
            )
            .map_err(|e| format!("{e}"))?;

            let mut columns: Option<Vec<String>> = None;
            let mut data: Vec<Vec<String>> = Vec::new();
            for row_result in cursor {
                let row = row_result.map_err(|e| format!("{e}"))?;
                let count = <_ as Row<Sqlite>>::field_count(&row);

                if columns.is_none() {
                    let mut hdr = Vec::with_capacity(count);
                    for i in 0..count {
                        let name = row
                            .get(i)
                            .and_then(|f| f.field_name().map(str::to_string))
                            .unwrap_or_default();
                        hdr.push(name);
                    }
                    columns = Some(hdr);
                }

                let mut row_strs = Vec::with_capacity(count);
                for i in 0..count {
                    row_strs.push(render_field(&row, i));
                }
                data.push(row_strs);
            }
            Ok((columns, data))
        };

        let (columns, data) = captured?;

        if let Some(columns) = columns {
            let elapsed_ms = performance_now() - start;
            return Ok(QueryOutcome::Rows {
                result: QueryRows { columns, rows: data },
                elapsed_ms,
            });
        }

        // No row-producing statement. Re-run as a batch so the
        // statement's side effects are applied, then ask SQLite how
        // many rows the last statement touched.
        conn.batch_execute(sql).map_err(|e| format!("{e}"))?;
        let changes: ChangesRow = diesel::sql_query("SELECT changes() AS n")
            .get_result(conn)
            .map_err(|e| format!("changes(): {e}"))?;
        let elapsed_ms = performance_now() - start;
        Ok(QueryOutcome::Affected { rows: changes.n, elapsed_ms })
    });

    match outcome {
        Ok(o) => o,
        Err(e) => QueryOutcome::Error(e),
    }
}

#[derive(QueryableByName)]
struct ChangesRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

fn render_field<'a, R: Row<'a, Sqlite>>(row: &R, idx: usize) -> String {
    let Some(field) = row.get(idx) else {
        return "NULL".into();
    };
    let Some(mut val) = field.value() else {
        return "NULL".into();
    };

    match val.value_type() {
        None => "NULL".into(),
        Some(SqliteType::Text) => val.read_text().to_string(),
        Some(SqliteType::Long | SqliteType::Integer | SqliteType::SmallInt) => {
            val.read_long().to_string()
        }
        Some(SqliteType::Double | SqliteType::Float) => format_double(val.read_double()),
        Some(SqliteType::Binary) => {
            let bytes = val.read_blob();
            format!("BLOB({} bytes)", bytes.len())
        }
    }
}

fn format_double(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e16 { format!("{v:.0}") } else { format!("{v}") }
}

fn performance_now() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}
