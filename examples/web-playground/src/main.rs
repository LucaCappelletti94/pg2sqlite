//! Browser WASM playground for pg2sqlite: translate, execute, and reverse
//! PostgreSQL/SQLite SQL in the browser.

mod components;
mod db;
mod runner;
mod samples;
mod state;
mod translator;

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::{
        fa_brands_icons::FaGithub,
        fa_solid_icons::{FaArrowRightArrowLeft, FaHeart},
    },
};
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

use crate::{
    components::{
        brand::{PostgresLogo, SqliteLogo},
        input_pane::InputPane,
        options_panel::OptionsPanel,
        output_pane::OutputPane,
        query_panel::QueryPanel,
        reverse_panel::ReversePanel,
        sample_picker::SamplePicker,
    },
    state::AppState,
};

const DEBOUNCE_MS: u32 = 700;
const REPO_URL: &str = "https://github.com/LucaCappelletti94/pg2sqlite";
const SPONSOR_URL: &str = "https://github.com/sponsors/LucaCappelletti94";

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    log::info!("pg2sqlite web demo booting");
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    // `use_context_provider` makes state reachable from any descendant without
    // prop-drilling.
    let state = use_context_provider(AppState::new);

    // Auto-translate: schedule a translation 700ms after the last edit, or
    // right away when `translate_now` moved (discrete edits). The snapshot
    // comparison in the spawned task discards a stale debounced run.
    let mut seen_translate_now = use_signal(|| 0u32);
    use_effect(move || {
        let input_snapshot = state.pg_input.read().clone();
        let opts_snapshot = state.options.read().clone();
        let generation = *state.translate_now.read();
        // Peek: subscribing would re-run the effect on its own write below.
        let immediate = generation != *seen_translate_now.peek();
        if immediate {
            seen_translate_now.set(generation);
        }

        spawn_local(async move {
            if !immediate {
                TimeoutFuture::new(DEBOUNCE_MS).await;
                // If the user typed more since the timer was set, a newer task owns this input.
                let current_input = state.pg_input.peek().clone();
                let current_opts = state.options.peek().clone();
                if current_input != input_snapshot || current_opts != opts_snapshot {
                    return;
                }
            }
            run_translate(state, &input_snapshot, &opts_snapshot);
        });
    });

    rsx! {
        // Icons and manifest live here rather than a custom index.html so dx keeps generating the head.
        document::Link { rel: "icon", href: "/favicon.ico", sizes: "any" }
        document::Link { rel: "icon", r#type: "image/svg+xml", href: "/pg2sqlite.svg" }
        document::Link { rel: "icon", r#type: "image/png", sizes: "32x32", href: "/favicon-32.png" }
        document::Link { rel: "icon", r#type: "image/png", sizes: "16x16", href: "/favicon-16.png" }
        document::Link { rel: "apple-touch-icon", sizes: "180x180", href: "/apple-touch-icon.png" }
        document::Link { rel: "manifest", href: "/manifest.json" }
        document::Meta { name: "theme-color", content: "#336791" }
        document::Meta {
            name: "description",
            content: "Translate PostgreSQL DDL and queries into runnable SQLite, execute the result against an in-page database, and translate SQLite DML back to PostgreSQL. Entirely in the browser.",
        }
        main { class: "app",
            AppHeader {}
            div { class: "editors-row",
                InputPane {}
                OutputPane {}
            }
            QueryPanel {}
            ReversePanel {}
        }
    }
}

#[component]
fn AppHeader() -> Element {
    let state: AppState = use_context();
    // Sponsor heart pulses after a translation lands, so the ask follows the tool
    // doing something.
    let translated = state.sqlite_output.read().is_some();

    rsx! {
        header { class: "app-header",
            div { class: "app-header-titles",
                div { class: "app-title-row",
                    h1 { class: "app-title",
                        PostgresLogo {}
                        Icon {
                            width: 22,
                            height: 22,
                            icon: FaArrowRightArrowLeft,
                            class: "title-arrow".to_string(),
                        }
                        SqliteLogo {}
                        span { class: "app-title-text", "pg2sqlite" }
                    }
                    nav { class: "topbar-actions", aria_label: "Project resources",
                        a {
                            class: "icon-link",
                            href: REPO_URL,
                            target: "_blank",
                            rel: "noopener noreferrer",
                            title: "pg2sqlite on GitHub. Opens in a new tab.",
                            "aria-label": "pg2sqlite on GitHub. Opens in a new tab.",
                            Icon { width: 16, height: 16, icon: FaGithub }
                        }
                        a {
                            class: if translated { "icon-link heartbtn heart-attention" } else { "icon-link heartbtn" },
                            href: SPONSOR_URL,
                            target: "_blank",
                            rel: "noopener noreferrer",
                            title: "Support this project. Opens in a new tab.",
                            "aria-label": "Support this project. Opens in a new tab.",
                            Icon { width: 16, height: 16, icon: FaHeart }
                        }
                    }
                }
                p { class: "app-tagline",
                    "Translate PostgreSQL to SQLite and execute the result in the browser."
                }
            }
            SamplePicker {}
            OptionsPanel {}
        }
    }
}

fn run_translate(state: AppState, pg_sql: &str, opts: &state::WebOptions) {
    // Empty input clears any prior output but doesn't drive an error.
    if pg_sql.trim().is_empty() {
        state.sqlite_output.clone().set(None);
        state.translation_error.clone().set(None);
        state.apply_error.clone().set(None);
        state.apply_ok.clone().set(false);
        state.stats.clone().set(None);
        state.query_result.clone().set(None);
        state.reverse_output.clone().set(None);
        return;
    }

    let options = opts.to_options();
    let now = || web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now());

    match translator::translate(pg_sql, &options, now) {
        Ok(output) => {
            let sqlite_sql = output.sqlite_sql;
            state.sqlite_output.clone().set(Some(sqlite_sql.clone()));
            state.translation_error.clone().set(None);
            state.stats.clone().set(Some(output.stats));
            state.query_result.clone().set(None);

            let session_var_funcs: Vec<String> =
                opts.session_variables.iter().map(|m| m.sqlite_function.clone()).collect();
            let apply_result =
                db::reopen(&session_var_funcs).and_then(|()| db::run_script(&sqlite_sql));
            match apply_result {
                Ok(()) => {
                    state.apply_error.clone().set(None);
                    state.apply_ok.clone().set(true);
                }
                Err(e) => {
                    log::warn!("apply to in-memory SQLite failed: {e}");
                    state.apply_error.clone().set(Some(e));
                    state.apply_ok.clone().set(false);
                }
            }
        }
        Err(err) => {
            state.sqlite_output.clone().set(None);
            state.translation_error.clone().set(Some(err));
            state.apply_error.clone().set(None);
            state.apply_ok.clone().set(false);
            state.stats.clone().set(None);
            state.query_result.clone().set(None);
            state.reverse_output.clone().set(None);
        }
    }
}
