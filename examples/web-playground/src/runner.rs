//! Run user SQL against the in-memory Diesel connection and return rows or
//! affected-row count.

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
    /// SELECT-style result.
    Rows {
        result: QueryRows,
        elapsed_ms: f64,
    },
    /// DDL/DML with no rows. `rows` is SQLite `changes()` for the last
    /// statement.
    Affected {
        rows: i64,
        elapsed_ms: f64,
    },
    Error(String),
}

/// Run a SQL script against the in-memory connection, returning rows or an
/// affected-row count.
pub fn run(sql: &str) -> QueryOutcome {
    let start = performance_now();

    type RawCapture = Result<(Option<Vec<String>>, Vec<Vec<String>>), String>;

    let outcome = db::with_conn(|conn| -> Result<QueryOutcome, String> {
        // Drain the cursor in a nested scope to drop the `&mut conn` borrow before the
        // `changes()` fallback re-borrows it.
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

        // No rows produced. Re-run to apply side effects, then ask SQLite for
        // changes().
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
