//! Sample-picker badges that seed the PG editor and Options form from a curated
//! list. The active badge is derived from the editor contents, so it
//! self-clears when the user types.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::fa_solid_icons::{
        FaBrain, FaListCheck, FaMagnifyingGlass, FaMapLocationDot, FaShieldHalved, FaTable,
        FaWandSparkles,
    },
};

use crate::{
    samples::{SAMPLES, SampleIcon, find_sample, find_sample_by_sql},
    state::AppState,
};

#[component]
pub fn SamplePicker() -> Element {
    let state: AppState = use_context();
    let active = find_sample_by_sql(&state.pg_input.read()).map(|s| s.name);

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
                    SampleBadge {
                        name: sample.name,
                        icon: sample.icon,
                        active: active == Some(sample.name),
                    }
                }
            }
        }
    }
}

#[component]
fn SampleBadge(name: &'static str, icon: SampleIcon, active: bool) -> Element {
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
        // The editor no longer holds the uploaded migrations, so drop the file
        // list rather than leave a stale order the user could drag.
        state.input_files.clone().set(Vec::new());
    };

    rsx! {
        button {
            r#type: "button",
            class: if active { "sample-badge is-active" } else { "sample-badge" },
            "aria-pressed": active,
            onclick: on_click,
            SampleBadgeIcon { icon }
            "{name}"
        }
    }
}

#[component]
fn SampleBadgeIcon(icon: SampleIcon) -> Element {
    let class = "badge-icon".to_string();
    match icon {
        SampleIcon::Table => rsx! { Icon { width: 14, height: 14, icon: FaTable, class } },
        SampleIcon::FullText => {
            rsx! { Icon { width: 14, height: 14, icon: FaMagnifyingGlass, class } }
        }
        SampleIcon::Vector => rsx! { Icon { width: 14, height: 14, icon: FaBrain, class } },
        SampleIcon::Geometry => {
            rsx! { Icon { width: 14, height: 14, icon: FaMapLocationDot, class } }
        }
        SampleIcon::Policy => rsx! { Icon { width: 14, height: 14, icon: FaShieldHalved, class } },
        SampleIcon::Constraint => rsx! { Icon { width: 14, height: 14, icon: FaListCheck, class } },
    }
}
