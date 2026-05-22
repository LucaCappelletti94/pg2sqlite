//! PostgreSQL and SQLite brand-logo helpers.
//!
//! Backed by `dx_icons::simple` (Simple Icons brand SVGs as Dioxus
//! components). We pin each glyph to its project's brand colour
//! (PG blue `#336791`, SQLite navy `#003B57`) so the marks stay
//! on-brand regardless of the surrounding text colour.

use dioxus::prelude::*;
use dx_icons::simple::{Icon, SimpleIcon};

#[component]
pub fn PostgresLogo() -> Element {
    rsx! {
        Icon {
            icon: SimpleIcon::Postgresql,
            size: 18,
            color: "#336791".to_string(),
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
            color: "#003B57".to_string(),
            class: "brand-logo brand-logo-sqlite".to_string(),
        }
    }
}
