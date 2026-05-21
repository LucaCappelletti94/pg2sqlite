//! Renders a `QueryRows` (Vec<Vec<String>>) as an HTML table.
//!
//! Deliberately minimal: no virtualisation, no sorting, no column
//! resizing. The use case is "run a SELECT against your translated
//! schema, see what comes back" — for genuinely large result sets
//! users should `LIMIT` in their query.

use dioxus::prelude::*;

use crate::runner::QueryRows;

#[component]
pub fn ResultsTable(result: QueryRows) -> Element {
    if result.rows.is_empty() {
        return rsx! {
            p { class: "results-empty", "No rows." }
        };
    }

    rsx! {
        div { class: "results-table-wrap",
            table { class: "results-table",
                thead {
                    tr {
                        for col in result.columns.iter() {
                            th { "{col}" }
                        }
                    }
                }
                tbody {
                    for row in result.rows.iter() {
                        tr {
                            for cell in row.iter() {
                                td { "{cell}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
