//! Left-hand pane: PostgreSQL editor + inline translation error.
//!
//! Sits side-by-side with `OutputPane` (the SQLite view). Translation
//! is automatic - the debounce + dispatch lives in `main.rs::App`,
//! which watches `pg_input` + `options` and schedules a translation
//! 700ms after the last change. The button-driven flow is gone, so
//! this pane is just an editor + an error card.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{FaDatabase, FaTriangleExclamation},
};

use crate::{
    components::editor::SqlEditor,
    state::{AppState, TranslationError},
};

#[component]
pub fn InputPane() -> Element {
    let state: AppState = use_context();
    let pg_input_text = state.pg_input.read().clone();
    let error_card = state.translation_error.read().clone();

    rsx! {
        section { class: "pane pane-input",
            header { class: "pane-header",
                h2 { class: "pane-title",
                    Icon {
                        width: 18,
                        height: 18,
                        icon: FaDatabase,
                        class: "title-icon".to_string(),
                    }
                    "PostgreSQL"
                }
            }
            SqlEditor {
                value: pg_input_text,
                aria_label: "PostgreSQL schema".to_string(),
                oninput: move |v| state.pg_input.clone().set(v),
            }
            if let Some(err) = error_card {
                ErrorCard { error: err }
            }
        }
    }
}

#[component]
fn ErrorCard(error: TranslationError) -> Element {
    rsx! {
        div { class: "error-card",
            div { class: "error-header",
                Icon { width: 16, height: 16, icon: FaTriangleExclamation, class: "error-icon".to_string() }
                span { class: "error-badge", "{error.category.label()}" }
            }
            pre { class: "error-message", "{error.message}" }
        }
    }
}
