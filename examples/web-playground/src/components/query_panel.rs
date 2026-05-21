//! Step 3: query the populated in-memory SQLite.
//!
//! Reveals after the Step 1 translation + apply both succeeded. The
//! user types SQL into the query box, picks a dialect (PG default,
//! SQLite alternative), and hits Run. PG inputs go through
//! `pg2sqlite::Pg2Sqlite::translate` first so `current_setting(...)`,
//! `ST_*`, etc. just work; SQLite inputs are passed verbatim. A
//! "Translated as:" line above the result table shows what actually
//! ran whenever the PG path rewrote the input.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaPlay, FaTriangleExclamation},
};

use crate::{
    components::{
        editor::SqlEditor,
        map::{Map, extract_lonlat},
        results_table::ResultsTable,
    },
    runner::{self, QueryOutcome},
    state::{AppState, QueryDialect, QueryDisplay},
    translator,
};

#[component]
pub fn QueryPanel() -> Element {
    let state: AppState = use_context();

    // The query panel reveals only when the translated schema has
    // actually been applied to the in-memory SQLite. Otherwise the
    // user could type a SELECT against a DB whose tables don't exist.
    if !*state.apply_ok.read() {
        return rsx! {};
    }

    let query = state.query_input.read().clone();
    let run_disabled = query.trim().is_empty();
    let display = state.query_result.read().clone();

    let on_run = move |_| run_query(state);

    rsx! {
        section { class: "panel query-panel",
            div { class: "panel-header",
                h2 { class: "panel-title", "Query the populated database" }
                DialectToggle {}
            }

            SqlEditor {
                value: query,
                aria_label: "Query SQL".to_string(),
                oninput: move |v| state.query_input.clone().set(v),
            }

            div { class: "query-panel-actions",
                button {
                    class: "primary",
                    disabled: run_disabled,
                    onclick: on_run,
                    Icon { width: 14, height: 14, icon: FaPlay, class: "btn-icon".to_string() }
                    " Run"
                }
            }

            if let Some(display) = display {
                QueryDisplayCard { display: display }
            }
        }
    }
}

/// PG / SQLite dialect toggle. Borrowed from `<input type="radio">`
/// rather than a button group so the active state shows obviously
/// even without bespoke styling.
#[component]
fn DialectToggle() -> Element {
    let state: AppState = use_context();
    let dialect = *state.query_dialect.read();

    rsx! {
        div { class: "dialect-toggle",
            label {
                input {
                    r#type: "radio",
                    name: "query-dialect",
                    checked: dialect == QueryDialect::Postgres,
                    onchange: move |_| state.query_dialect.clone().set(QueryDialect::Postgres),
                }
                " PostgreSQL"
            }
            label {
                input {
                    r#type: "radio",
                    name: "query-dialect",
                    checked: dialect == QueryDialect::Sqlite,
                    onchange: move |_| state.query_dialect.clone().set(QueryDialect::Sqlite),
                }
                " SQLite"
            }
        }
    }
}

/// Renders the "Translated as: ..." line (PG dialect only) plus
/// either a results table, an "Affected N rows" line, or an error
/// card.
#[component]
fn QueryDisplayCard(display: QueryDisplay) -> Element {
    let state: AppState = use_context();
    let dialect = *state.query_dialect.read();
    let show_rewrite = dialect == QueryDialect::Postgres
        && state.query_input.read().as_str() != display.effective_sql;

    rsx! {
        div { class: "query-display",
            if show_rewrite {
                div { class: "query-rewrite",
                    span { class: "query-rewrite-label", "Translated as:" }
                    code { class: "query-rewrite-sql", "{display.effective_sql}" }
                }
            }
            match display.outcome {
                QueryOutcome::Rows { result, elapsed_ms } => {
                    // Map plots only when geolite is configured (the
                    // PostGIS flag in the Options form is a clean cue
                    // that the user is doing spatial work) and the
                    // result actually exposes `lon` / `lat` columns.
                    // Otherwise we'd be plotting random numeric pairs.
                    let geolite = state.options.read().geolite_enabled;
                    let points = if geolite {
                        extract_lonlat(&result.columns, &result.rows)
                    } else {
                        Vec::new()
                    };
                    rsx! {
                        p { class: "query-stats",
                            "Returned {result.rows.len()} row"
                            if result.rows.len() != 1 { "s" }
                            " in {format_ms(elapsed_ms)} ms."
                        }
                        ResultsTable { result: result }
                        if !points.is_empty() {
                            Map { points: points }
                        }
                    }
                },
                QueryOutcome::Affected { rows, elapsed_ms } => rsx! {
                    p { class: "query-stats",
                        "Affected {rows} row"
                        if rows != 1 { "s" }
                        " in {format_ms(elapsed_ms)} ms."
                    }
                },
                QueryOutcome::Error(msg) => rsx! {
                    div { class: "query-error",
                        div { class: "query-error-header",
                            Icon { width: 14, height: 14, icon: FaTriangleExclamation, class: "btn-icon".to_string() }
                            span { class: "query-error-badge", "SQLite runtime" }
                        }
                        pre { class: "query-error-message", "{msg}" }
                    }
                },
            }
        }
    }
}

fn format_ms(ms: f64) -> String {
    if ms < 1.0 { format!("{ms:.2}") } else { format!("{ms:.1}") }
}

/// Run the user's query against the in-memory connection, dispatching
/// PG inputs through pg2sqlite first.
fn run_query(state: AppState) {
    let raw_query = state.query_input.read().clone();
    let dialect = *state.query_dialect.read();

    let effective_sql = match dialect {
        QueryDialect::Sqlite => raw_query.clone(),
        QueryDialect::Postgres => {
            // Reuse the same options the schema was translated under.
            // Without this, PG queries that depend on session-variable
            // mappings or geolite enablement would silently behave
            // differently from the schema they're querying.
            let options = state.options.read().to_options();
            let now = || web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now());
            match translator::translate(&raw_query, &options, now) {
                Ok(translated) => translated.sqlite_sql,
                Err(err) => {
                    state.query_result.clone().set(Some(QueryDisplay {
                        effective_sql: raw_query.clone(),
                        outcome: QueryOutcome::Error(format!(
                            "Translation failed ({}): {}",
                            err.category.label(),
                            err.message
                        )),
                    }));
                    return;
                }
            }
        }
    };

    let outcome = runner::run(&effective_sql);
    state.query_result.clone().set(Some(QueryDisplay { effective_sql, outcome }));
}
