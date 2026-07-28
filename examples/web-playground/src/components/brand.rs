//! Brand logo helpers. Palette lives in `app.css` so marks work in dark mode
//! without baking hex here.

use dioxus::prelude::*;
use dx_icons::simple::{Icon, SimpleIcon};

#[component]
pub fn PostgresLogo() -> Element {
    rsx! {
        Icon {
            icon: SimpleIcon::Postgresql,
            size: 18,
            class: "brand-logo brand-logo-postgres".to_string(),
        }
    }
}

#[component]
pub fn SqliteLogo() -> Element {
    rsx! {
        Icon {
            icon: SimpleIcon::Sqlite,
            size: 18,
            class: "brand-logo brand-logo-sqlite".to_string(),
        }
    }
}
