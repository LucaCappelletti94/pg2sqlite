//! Step 4 (collapsed): reverse-translate SQLite DML back to PostgreSQL.
//!
//! Sits at the bottom of the page inside a `<details>` so it doesn't
//! distract first-time visitors. Becomes available once Step 2
//! succeeded (we need a schema to reverse against). The schema is
//! rebuilt from the live PG editor contents every time the user hits
//! Run rather than carried through state - the PG editor + options
//! form is the single source of truth.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaBan, FaCircleCheck, FaPlay, FaRightLeft, FaTriangleExclamation},
};

use crate::{
    components::{
        brand::PostgresLogo,
        editor::{SqlEditor, SqlViewer},
    },
    samples::{SampleQueryKind, find_sample_by_sql},
    state::{AppState, ReverseOutcome},
    translator,
};

#[component]
pub fn ReversePanel() -> Element {
    let state: AppState = use_context();

    // Available once a forward translation has succeeded - we need a
    // schema to reverse against. If the PG side is currently in an
    // error state, the most recent successful schema is gone, so the
    // panel hides itself rather than reverse against stale data.
    if state.sqlite_output.read().is_none() {
        return rsx! {};
    }

    let sqlite_input = state.reverse_input.read().clone();
    let outcome = state.reverse_output.read().clone();
    let run_disabled = sqlite_input.trim().is_empty();

    let on_run = move |_| run_reverse(state);

    rsx! {
        details { class: "reverse-panel",
            summary {
                Icon {
                    width: 14,
                    height: 14,
                    icon: FaRightLeft,
                    class: "summary-icon".to_string(),
                }
                "Reverse translation (SQLite to PostgreSQL)"
            }
            div { class: "reverse-panel-body",
                ReverseSampleQueriesStrip {}

                SqlEditor {
                    value: sqlite_input,
                    aria_label: "SQLite DML to reverse-translate".to_string(),
                    oninput: move |v| state.reverse_input.clone().set(v),
                }

                div { class: "reverse-panel-actions",
                    button {
                        class: "primary",
                        disabled: run_disabled,
                        onclick: on_run,
                        Icon { width: 14, height: 14, icon: FaPlay, class: "btn-icon".to_string() }
                        " Reverse translate"
                    }
                }

                if let Some(outcome) = outcome {
                    ReverseResult { outcome: outcome }
                }
            }
        }
    }
}

#[component]
fn ReverseResult(outcome: ReverseOutcome) -> Element {
    match outcome {
        Ok(pg_sql) => {
            rsx! {
                div { class: "reverse-result",
                    h3 { class: "reverse-result-title",
                        PostgresLogo {}
                        "PostgreSQL output"
                    }
                    SqlViewer {
                        value: pg_sql,
                        aria_label: "Reverse-translated PostgreSQL SQL".to_string(),
                    }
                }
            }
        }
        Err(err) => {
            rsx! {
                div { class: "error-card",
                    div { class: "error-header",
                        Icon { width: 16, height: 16, icon: FaTriangleExclamation, class: "error-icon".to_string() }
                        span { class: "error-badge", "{err.category.label()}" }
                    }
                    pre { class: "error-message", "{err.message}" }
                }
            }
        }
    }
}

fn run_reverse(state: AppState) {
    let pg_schema = state.pg_input.read().clone();
    let sqlite_input = state.reverse_input.read().clone();
    let options = state.options.read().to_options();

    let result = translator::reverse_translate(&sqlite_input, &pg_schema, &options);
    state.reverse_output.clone().set(Some(result));
}

/// Renders one chip per curated reverse query attached to the
/// currently-loaded sample. Hides itself as soon as the user edits the
/// PG input (because the lookup is exact-equality), so once the schema
/// has drifted the suggestions can no longer be assumed to match.
#[component]
fn ReverseSampleQueriesStrip() -> Element {
    let state: AppState = use_context();
    let pg_input = state.pg_input.read().clone();
    let Some(sample) = find_sample_by_sql(&pg_input) else {
        return rsx! {};
    };
    if sample.reverse_queries.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "sample-queries",
            span { class: "sample-queries-label", "Try a reverse translation" }
            div { class: "sample-queries-chips",
                for query in sample.reverse_queries.iter() {
                    ReverseSampleQueryChip {
                        label: query.label,
                        sql: query.sql,
                        kind: query.kind,
                    }
                }
            }
        }
    }
}

#[component]
fn ReverseSampleQueryChip(
    label: &'static str,
    sql: &'static str,
    kind: SampleQueryKind,
) -> Element {
    let state: AppState = use_context();
    let class = match kind {
        SampleQueryKind::Positive => "sample-query-chip",
        SampleQueryKind::Negative => "sample-query-chip negative",
    };
    let on_click = move |_| {
        state.reverse_input.clone().set(sql.to_string());
        run_reverse(state);
    };

    rsx! {
        button {
            r#type: "button",
            class: class,
            title: "{sql}",
            onclick: on_click,
            match kind {
                SampleQueryKind::Positive => rsx! {
                    Icon { width: 12, height: 12, icon: FaCircleCheck, class: "chip-icon".to_string() }
                },
                SampleQueryKind::Negative => rsx! {
                    Icon { width: 12, height: 12, icon: FaBan, class: "chip-icon".to_string() }
                },
            }
            "{label}"
        }
    }
}
