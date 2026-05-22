//! Right-hand pane: read-only SQLite output + Copy + Download +
//! apply status.
//!
//! Always rendered. When there's no successful translation yet, the
//! pane shows a placeholder so the page layout doesn't jump around as
//! the user types in the left editor. Once a translation lands the
//! editor populates and the action buttons activate.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaCopy, FaDownload, FaTriangleExclamation},
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

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
    let download_sql = sqlite_sql.clone().unwrap_or_default();

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
                            class: "secondary",
                            disabled: !has_output,
                            title: "Copy SQLite SQL to clipboard",
                            onclick: move |_| copy_to_clipboard(copy_sql.clone()),
                            Icon { width: 14, height: 14, icon: FaCopy, class: "btn-icon".to_string() }
                            " Copy"
                        }
                        button {
                            class: "secondary",
                            disabled: !has_output,
                            title: "Download as pg2sqlite-output.sql",
                            onclick: move |_| download_blob(&download_sql, "pg2sqlite-output.sql"),
                            Icon { width: 14, height: 14, icon: FaDownload, class: "btn-icon".to_string() }
                            " Download"
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

/// Async copy via the Clipboard API. Fire-and-forget: failure is
/// logged but not surfaced (rejections are usually permission-related
/// and the user can manually select the editor contents as a
/// fallback).
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

/// Build a Blob, hand it a temporary object URL, click a synthetic
/// `<a download>` to trigger the file save, then revoke the URL.
fn download_blob(text: &str, filename: &str) {
    let document = web_sys::window().and_then(|w| w.document());
    let Some(document) = document else {
        log::error!("no document object");
        return;
    };

    let bytes = js_sys::Array::new();
    bytes.push(&wasm_bindgen::JsValue::from_str(text));
    let bag = BlobPropertyBag::new();
    bag.set_type("application/sql;charset=utf-8");

    let blob = match Blob::new_with_str_sequence_and_options(&bytes, &bag) {
        Ok(b) => b,
        Err(e) => {
            log::error!("blob construction failed: {e:?}");
            return;
        }
    };
    let url = match Url::create_object_url_with_blob(&blob) {
        Ok(u) => u,
        Err(e) => {
            log::error!("object URL creation failed: {e:?}");
            return;
        }
    };

    let anchor =
        document.create_element("a").ok().and_then(|el| el.dyn_into::<HtmlAnchorElement>().ok());
    if let Some(anchor) = anchor {
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.click();
    } else {
        log::error!("could not create <a> for download");
    }

    let _ = Url::revoke_object_url(&url);
}
