//! pg2sqlite - PostgreSQL to SQLite, in your browser.
//!
//! This is the project's demo + landing page, served as a single
//! static WASM bundle (`dx bundle --platform web --release`). Layout:
//!
//! - Header bar: project name + tagline + sample picker + Advanced options
//!   `<details>`.
//! - Two-column body: PostgreSQL editor on the left, SQLite output on the
//!   right. Translation fires 700ms after the last edit; the right pane updates
//!   in place.
//! - Below, when the translated SQL has been applied to the in-memory SQLite:
//!   the Query panel, with a PG / SQLite dialect toggle. PG queries are
//!   forward-translated by pg2sqlite before they hit the in-page database.
//! - Below that, when a forward translation has succeeded: the
//!   reverse-translation `<details>` (SQLite -> PG).
//! - Inside the query results: the PostGIS map appears whenever the result row
//!   carries `lon` / `lat` columns.

mod components;
mod db;
mod runner;
mod samples;
mod state;
mod translator;

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::{fa_brands_icons::FaGithub, fa_solid_icons::FaArrowRightArrowLeft},
};
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

use crate::{
    components::{
        input_pane::InputPane, options_panel::OptionsPanel, output_pane::OutputPane,
        query_panel::QueryPanel, reverse_panel::ReversePanel, sample_picker::SamplePicker,
    },
    state::AppState,
};

const DEBOUNCE_MS: u32 = 700;

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    log::info!("pg2sqlite web demo booting");
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    // Constructed once at mount; `use_context_provider` makes the
    // bag of signals reachable from any descendant without
    // prop-drilling.
    let state = use_context_provider(AppState::new);

    // Auto-translate watcher. Every time the PG input or the options
    // change, schedule a translation 700ms later. If another change
    // arrives before the timer fires, the snapshot comparison inside
    // the spawned task supersedes the older attempt - so only the
    // edit-stable input ever reaches pg2sqlite.
    use_effect(move || {
        // Read each signal so the effect re-runs when either changes.
        let input_snapshot = state.pg_input.read().clone();
        let opts_snapshot = state.options.read().clone();

        spawn_local(async move {
            TimeoutFuture::new(DEBOUNCE_MS).await;
            // Re-read; if the user has typed more in the interim, a
            // newer effect run has scheduled its own task and this
            // one is stale.
            let current_input = state.pg_input.peek().clone();
            let current_opts = state.options.peek().clone();
            if current_input != input_snapshot || current_opts != opts_snapshot {
                return;
            }
            run_translate(state, &input_snapshot, &opts_snapshot);
        });
    });

    rsx! {
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
    rsx! {
        header { class: "app-header",
            div { class: "app-header-titles",
                h1 { class: "app-title",
                    Icon {
                        width: 28,
                        height: 28,
                        icon: FaArrowRightArrowLeft,
                        class: "title-icon".to_string(),
                    }
                    "pg2sqlite"
                }
                p { class: "app-tagline",
                    "Translate PostgreSQL to SQLite. In your browser. No backend."
                }
            }
            div { class: "app-header-controls",
                SamplePicker {}
                a {
                    class: "brand-link",
                    href: "https://github.com/LucaCappelletti94/pg2sqlite",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    title: "View pg2sqlite on GitHub",
                    Icon {
                        width: 22,
                        height: 22,
                        icon: FaGithub,
                        class: "brand-icon".to_string(),
                    }
                }
            }
            OptionsPanel {}
        }
    }
}

/// Single-pass forward translation + in-memory apply. Mirrors what
/// the explicit Translate button used to do, just driven by the
/// debounce effect instead of a click handler.
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
