//! Renders a `QueryRows` as an HTML table.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::fa_solid_icons::FaInbox};

use crate::runner::QueryRows;

#[component]
pub fn ResultsTable(result: QueryRows) -> Element {
    if result.rows.is_empty() {
        return rsx! {
            p { class: "results-empty",
                Icon {
                    width: 14,
                    height: 14,
                    icon: FaInbox,
                    class: "label-icon".to_string(),
                }
                "No rows."
            }
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
