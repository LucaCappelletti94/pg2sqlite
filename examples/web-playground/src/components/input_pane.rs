//! Left-hand pane: PostgreSQL editor, migration upload, and inline error.
//! Uploaded files are concatenated in apply order because later migrations
//! reference tables from earlier ones.

use dioxus::{html::FileData, prelude::*};
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{
        FaFolderOpen, FaGripVertical, FaTriangleExclamation, FaUpload, FaXmark,
    },
};

use crate::{
    components::{brand::PostgresLogo, editor::SqlEditor},
    state::{AppState, TranslationError},
};

#[component]
pub fn InputPane() -> Element {
    let state: AppState = use_context();
    // Drag source index. Transient pointer state, so it stays local to the pane
    // rather than joining the shared signal bag.
    let mut drag_from = use_signal::<Option<usize>>(|| None);

    let pg_input_text = state.pg_input.read().clone();
    let error_card = state.translation_error.read().clone();
    let files = state.input_files.read().clone();

    rsx! {
        section { class: "pane pane-input",
            header { class: "pane-header",
                div { class: "pane-title-row",
                    h2 { class: "pane-title",
                        PostgresLogo {}
                        "PostgreSQL"
                    }
                    div { class: "pane-actions",
                        label {
                            class: "icon-link",
                            title: "Upload one or more .sql files, concatenated in filename order",
                            "aria-label": "Upload .sql files",
                            Icon { width: 16, height: 16, icon: FaUpload }
                            input {
                                class: "file-input",
                                r#type: "file",
                                accept: ".sql",
                                multiple: true,
                                "aria-label": "Upload one or more .sql files, ordered by filename",
                                onchange: move |evt| load_files(state, evt.files(), false),
                            }
                        }
                        label {
                            class: "icon-link",
                            title: "Upload a folder of migrations, concatenated in relative-path order",
                            "aria-label": "Upload a folder of migrations",
                            Icon { width: 16, height: 16, icon: FaFolderOpen }
                            input {
                                class: "file-input",
                                r#type: "file",
                                directory: true,
                                // A directory pick yields many files. Browsers
                                // imply this, declaring it keeps the element
                                // honest about what it returns.
                                multiple: true,
                                "aria-label": "Upload a folder of .sql migrations, ordered by relative path",
                                onchange: move |evt| load_files(state, evt.files(), true),
                            }
                        }
                    }
                }
            }

            if !files.is_empty() {
                div { class: "input-files",
                    span { class: "field-label", "Input files, applied top to bottom (drag to reorder)" }
                    ul { class: "file-list",
                        for (i , (name , _)) in files.iter().enumerate() {
                            li {
                                key: "{name}-{i}",
                                draggable: true,
                                ondragstart: move |_| drag_from.set(Some(i)),
                                ondragover: move |e| e.prevent_default(),
                                ondrop: move |e| {
                                    e.prevent_default();
                                    if let Some(from) = drag_from.take() {
                                        move_file(state, from, i);
                                    }
                                },
                                Icon {
                                    width: 12,
                                    height: 12,
                                    icon: FaGripVertical,
                                    class: "drag-handle".to_string(),
                                }
                                span { class: "file-order", "{i + 1}." }
                                span { class: "file-name", "{name}" }
                                button {
                                    class: "file-remove",
                                    r#type: "button",
                                    title: "Remove this file from the input",
                                    "aria-label": "Remove {name} from the input",
                                    onclick: move |_| remove_file(state, i),
                                    Icon { width: 12, height: 12, icon: FaXmark }
                                }
                            }
                        }
                    }
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

/// An empty pick (cancelled dialog or unreadable folder) leaves the current
/// input alone.
fn load_files(state: AppState, mut picked: Vec<FileData>, by_path: bool) {
    if by_path {
        picked.sort_by_key(FileData::path);
    } else {
        picked.sort_by_key(FileData::name);
    }
    spawn(async move {
        let mut collected: Vec<(String, String)> = Vec::with_capacity(picked.len());
        for file in &picked {
            if let Ok(text) = file.read_string().await {
                collected.push((file.name(), text));
            }
        }
        // An empty pick (cancelled dialog, or a folder with no readable file)
        // leaves the current input alone rather than blanking the editor.
        if !collected.is_empty() {
            state.pg_input.clone().set(concat_files(&collected));
            state.input_files.clone().set(collected);
            state.request_immediate_translation();
        }
    });
}

fn move_file(state: AppState, from: usize, to: usize) {
    let mut files = state.input_files;
    if from == to || from >= files.read().len() {
        return;
    }
    {
        let mut list = files.write();
        let item = list.remove(from);
        let at = to.min(list.len());
        list.insert(at, item);
    }
    state.pg_input.clone().set(concat_files(&files.read()));
    state.request_immediate_translation();
}

fn remove_file(state: AppState, index: usize) {
    let mut files = state.input_files;
    if index >= files.read().len() {
        return;
    }
    files.write().remove(index);
    state.pg_input.clone().set(concat_files(&files.read()));
    state.request_immediate_translation();
}

/// Concatenate file contents joined by a blank line so adjacent files stay
/// parseable.
fn concat_files(files: &[(String, String)]) -> String {
    files.iter().map(|(_, content)| content.as_str()).collect::<Vec<_>>().join("\n\n")
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
