//! Right-hand pane: read-only SQLite output, a copy button, and apply status.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaCopy, FaTriangleExclamation},
};
use wasm_bindgen_futures::JsFuture;

use crate::{
    components::{brand::SqliteLogo, editor::SqlViewer},
    state::AppState,
};

#[component]
pub fn OutputPane() -> Element {
    let state: AppState = use_context();
    let sqlite_sql = state.sqlite_output.read().clone();
    let stats = *state.stats.read();
    let apply_error = state.apply_error.read().clone();

    let has_output = sqlite_sql.is_some();
    let copy_sql = sqlite_sql.clone().unwrap_or_default();

    rsx! {
        section { class: "pane pane-output",
            header { class: "pane-header",
                div { class: "pane-title-row",
                    h2 { class: "pane-title",
                        SqliteLogo {}
                        "SQLite"
                    }
                    div { class: "pane-actions",
                        button {
                            class: "icon-link",
                            r#type: "button",
                            disabled: !has_output,
                            title: "Copy the SQLite SQL to the clipboard",
                            // The button carries no text, so it needs its own
                            // accessible name.
                            "aria-label": "Copy the SQLite SQL to the clipboard",
                            onclick: move |_| copy_to_clipboard(copy_sql.clone()),
                            Icon { width: 16, height: 16, icon: FaCopy }
                        }
                    }
                }
            }
            if let Some(sqlite_sql) = sqlite_sql {
                SqlViewer {
                    value: sqlite_sql,
                    aria_label: "Translated SQLite SQL".to_string(),
                }
                StatusLine { stats: stats, apply_error: apply_error }
            } else {
                div { class: "pane-placeholder",
                    "Pick a sample above, or paste your PostgreSQL on the left."
                }
            }
        }
    }
}

#[component]
fn StatusLine(
    stats: Option<crate::state::TranslationStats>,
    apply_error: Option<String>,
) -> Element {
    rsx! {
        if let Some(stats) = stats {
            p { class: "pane-status",
                "{stats.statement_count} statement"
                if stats.statement_count != 1 { "s" }
                " in {format_ms(stats.elapsed_ms)} ms"
            }
        }

        if let Some(apply_err) = apply_error {
            div { class: "apply-error-card",
                div { class: "apply-error-header",
                    Icon {
                        width: 14,
                        height: 14,
                        icon: FaTriangleExclamation,
                        class: "error-icon".to_string(),
                    }
                    " In-memory apply failed"
                }
                pre { class: "apply-error-message", "{apply_err}" }
            }
        }
    }
}

fn format_ms(ms: f64) -> String {
    if ms < 1.0 { format!("{ms:.2}") } else { format!("{ms:.1}") }
}

/// Fire-and-forget clipboard write. Failures are logged, not surfaced.
fn copy_to_clipboard(text: String) {
    let Some(window) = web_sys::window() else {
        log::error!("no window object");
        return;
    };
    let clipboard = window.navigator().clipboard();
    let promise = clipboard.write_text(&text);
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = JsFuture::from(promise).await {
            log::warn!("clipboard write failed: {e:?}");
        } else {
            log::info!("copied {} bytes to clipboard", text.len());
        }
    });
}
