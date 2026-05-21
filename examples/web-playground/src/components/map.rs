//! PostGIS map panel: equirectangular canvas that plots points from a
//! query result whenever the result exposes `lon` / `lat` columns.
//!
//! Why not parse WKB BLOBs directly? Two reasons. (1) WKB / EWKB
//! parsing inside the playground would duplicate logic that's already
//! the responsibility of geolite (`ST_X`, `ST_Y`); (2) it matches the
//! pattern sqlitegis uses - the user writes
//! `SELECT id, ST_X(geom) AS lon, ST_Y(geom) AS lat FROM places`
//! and the map renders the projected columns. This keeps the
//! playground side free of geometry-decoding code.
//!
//! The map is gated on `geolite_enabled` purely as a UX cue: it's
//! the option that signals "I have spatial data". Users can still
//! project lon/lat without geolite if they're just doing plain
//! double columns.

use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

const CANVAS_ID: &str = "pg2sqlite-map";
const CANVAS_W: u32 = 720;
const CANVAS_H: u32 = 360;
const POINT_RADIUS: f64 = 3.0;
const DOT_FILL: &str = "#F97316"; // amber/orange
const GRID_STROKE: &str = "#E5E7EB";
const COASTLINE_FILL: &str = "#F1F5F9";

#[component]
pub fn Map(points: Vec<(f64, f64)>) -> Element {
    // Draw whenever the point list changes. The effect runs after
    // mount, by which time the canvas is in the DOM.
    let points_for_effect = points.clone();
    use_effect(move || {
        draw(&points_for_effect);
    });

    rsx! {
        div { class: "map-wrap",
            canvas {
                id: CANVAS_ID,
                width: "{CANVAS_W}",
                height: "{CANVAS_H}",
                class: "map-canvas",
            }
            p { class: "map-caption",
                "Plotted {points.len()} point"
                if points.len() != 1 { "s" }
                " from the query result's "
                code { "lon" } " / " code { "lat" }
                " columns."
            }
        }
    }
}

/// Render the equirectangular world background + the supplied points.
/// Coordinate convention: lon in `[-180, 180]`, lat in `[-90, 90]`,
/// projected to canvas pixels with the standard equirectangular map
/// formula (x = (lon + 180) / 360 * W, y = (90 - lat) / 180 * H).
fn draw(points: &[(f64, f64)]) {
    let Some(ctx) = canvas_context() else { return };

    ctx.set_fill_style_str(COASTLINE_FILL);
    ctx.fill_rect(0.0, 0.0, f64::from(CANVAS_W), f64::from(CANVAS_H));

    // Reference grid: equator + prime meridian + 30 degree lines.
    ctx.set_stroke_style_str(GRID_STROKE);
    ctx.set_line_width(1.0);
    for lon_step in (-180..=180).step_by(30) {
        let x = project_lon(f64::from(lon_step));
        ctx.begin_path();
        ctx.move_to(x, 0.0);
        ctx.line_to(x, f64::from(CANVAS_H));
        ctx.stroke();
    }
    for lat_step in (-90..=90).step_by(30) {
        let y = project_lat(f64::from(lat_step));
        ctx.begin_path();
        ctx.move_to(0.0, y);
        ctx.line_to(f64::from(CANVAS_W), y);
        ctx.stroke();
    }

    // Highlighted points from the query result.
    ctx.set_fill_style_str(DOT_FILL);
    for &(lon, lat) in points {
        let x = project_lon(lon);
        let y = project_lat(lat);
        ctx.begin_path();
        let _ = ctx.arc(x, y, POINT_RADIUS, 0.0, core::f64::consts::TAU);
        ctx.fill();
    }
}

fn project_lon(lon: f64) -> f64 {
    ((lon + 180.0) / 360.0) * f64::from(CANVAS_W)
}

fn project_lat(lat: f64) -> f64 {
    ((90.0 - lat) / 180.0) * f64::from(CANVAS_H)
}

fn canvas_context() -> Option<CanvasRenderingContext2d> {
    let document = web_sys::window()?.document()?;
    let canvas = document.get_element_by_id(CANVAS_ID)?;
    let canvas: HtmlCanvasElement = canvas.dyn_into().ok()?;
    canvas.get_context("2d").ok()??.dyn_into().ok()
}

/// Pull `(lon, lat)` tuples out of a query result by inspecting its
/// column headers. Returns an empty Vec if either column is missing
/// or its cells don't parse as f64. The query panel uses this to
/// decide whether to render the map at all.
pub fn extract_lonlat(columns: &[String], rows: &[Vec<String>]) -> Vec<(f64, f64)> {
    let lon_idx = columns.iter().position(|c| c.eq_ignore_ascii_case("lon"));
    let lat_idx = columns.iter().position(|c| c.eq_ignore_ascii_case("lat"));
    let (Some(lon_idx), Some(lat_idx)) = (lon_idx, lat_idx) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let lon = row.get(lon_idx)?.parse::<f64>().ok()?;
            let lat = row.get(lat_idx)?.parse::<f64>().ok()?;
            Some((lon, lat))
        })
        .collect()
}
