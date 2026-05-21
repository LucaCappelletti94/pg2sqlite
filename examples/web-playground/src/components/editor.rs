//! Thin wrappers around `dioxus_code_editor::CodeEditor` so every SQL
//! pane in the playground has the same theme, line-numbers, and
//! spellcheck-off defaults without each call site repeating them.
//!
//! Build note: `dioxus-code-editor` pulls in `arborium-tree-sitter`,
//! which links cleanly only under cargo's `release` profile - the
//! `dev` profile's debug-info emission triggers an undefined-symbol
//! error from the SQL grammar's C wasm-shim. Use `dx serve --release`
//! / `dx build --release` when developing locally.

use dioxus::prelude::*;
use dioxus_code::{CodeTheme, Theme};
use dioxus_code_editor::{CodeEditor, Language};

fn sql_theme() -> CodeTheme {
    CodeTheme::fixed(Theme::GITHUB_LIGHT)
}

/// Read-write SQL editor. The parent owns the `Signal<String>` and
/// passes `value: signal()` + `oninput: move |v| signal.set(v)` in
/// the usual Dioxus pattern.
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

/// Read-only SQL viewer for the Step 2 output and the reverse panel.
/// `dioxus_code_editor`'s `CodeEditor` doesn't expose an explicit
/// read-only flag in 0.1, so we drop the `oninput` callback - edits
/// never reach the parent signal, and the parent never feeds the
/// editor a new value other than the freshly translated SQL.
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
