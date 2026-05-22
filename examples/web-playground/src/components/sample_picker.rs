//! Sample-picker badges that seed the PG editor + Options form from
//! a curated list. Each badge ("Simple" / "FTS5" / "pgvector" /
//! "PostGIS" / "RLS") is a button; clicking replaces both the
//! editor contents and the option-form state in one go. The
//! auto-translate watcher in `App` notices the input change and
//! runs a translation after the usual debounce, so the SQLite pane
//! updates without any additional click.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::fa_solid_icons::FaWandSparkles};

use crate::{
    samples::{SAMPLES, find_sample},
    state::AppState,
};

#[component]
pub fn SamplePicker() -> Element {
    rsx! {
        div { class: "sample-picker",
            span { class: "sample-picker-label",
                Icon {
                    width: 14,
                    height: 14,
                    icon: FaWandSparkles,
                    class: "label-icon".to_string(),
                }
                "Try a sample"
            }
            div { class: "sample-badges",
                for sample in SAMPLES {
                    SampleBadge { name: sample.name }
                }
            }
        }
    }
}

#[component]
fn SampleBadge(name: &'static str) -> Element {
    let state: AppState = use_context();

    let on_click = move |_| {
        let Some(sample) = find_sample(name) else {
            log::warn!("unknown sample name: {name}");
            return;
        };
        let mut pg_input = state.pg_input;
        let mut options = state.options;
        let mut opts = options.read().clone();
        (sample.apply_options)(&mut opts);
        // Set options first, then input, so the auto-translate
        // watcher observes both changes and re-translates with the
        // sample's pre-configured options.
        options.set(opts);
        pg_input.set(sample.sql.to_string());
    };

    rsx! {
        button {
            r#type: "button",
            class: "sample-badge",
            onclick: on_click,
            "{name}"
        }
    }
}
