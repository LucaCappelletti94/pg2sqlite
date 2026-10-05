//! SQL editor/viewer wrappers. arborium fails to link under the dev profile
//! (undefined `stderr`), use `--release`.

use dioxus::prelude::*;
use dioxus_code::{CodeTheme, Theme};
use dioxus_code_editor::{CodeEditor, Language};

fn sql_theme() -> CodeTheme {
    CodeTheme::system(Theme::GITHUB_LIGHT, Theme::GITHUB_DARK)
}

/// Read-write SQL editor backed by `CodeEditor`.
#[component]
pub fn SqlEditor(value: String, aria_label: String, oninput: EventHandler<String>) -> Element {
    rsx! {
        CodeEditor {
            value: value,
            language: Language::Sql,
            theme: sql_theme(),
            line_numbers: true,
            spellcheck: false,
            aria_label: aria_label,
            class: "editor".to_string(),
            oninput: move |v| oninput.call(v),
        }
    }
}

/// Read-only SQL viewer. `dioxus_code_editor` 0.1 has no read-only flag, so
/// `oninput` is dropped.
#[component]
pub fn SqlViewer(value: String, aria_label: String) -> Element {
    rsx! {
        CodeEditor {
            value: value,
            language: Language::Sql,
            theme: sql_theme(),
            line_numbers: true,
            spellcheck: false,
            aria_label: aria_label,
            class: "editor editor-readonly".to_string(),
            oninput: move |_| {},
        }
    }
}
