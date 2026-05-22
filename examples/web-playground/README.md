# pg2sqlite — live demo + project site

A Dioxus + sqlite-wasm-rs web app that runs entirely in the browser:
paste PostgreSQL DDL on the left, see the SQLite translation on the
right (updates 700ms after you stop typing), execute it against an
in-memory SQLite, and run queries against the populated schema in
either PG or SQLite dialect.

The app doubles as the project's landing page — it's the canonical
demo we link from the README and the crate metadata, and the live
URL exercises the same pg2sqlite build that ships on crates.io
(consumed here via `path = "../.."` with `default-features = false`
so the no_std + alloc build path is what users see).

## Why a separate crate?

Dioxus's transitive dep graph is heavy enough that we'd rather not
have it slow down the main pg2sqlite workspace's precommit / audit
checks. This crate is intentionally NOT a workspace member; it
points at pg2sqlite via a `path = "../.."` dep with
`default-features = false` so it inherits pg2sqlite's no_std + alloc
build (and the `[patch.crates-io]` sqlparser fork that makes the
`visitor` feature no_std-compatible).

## Run it

```bash
# from this directory
dx serve --platform web --release
```

Then visit <http://localhost:8080>. Hot reload is on for `src/` and
`public/`.

Use the `--release` profile: `dioxus-code-editor` pulls in
`arborium-tree-sitter`, whose C grammars currently fail to link
under cargo's `dev` profile (undefined `stderr` from the wasm-shim
header). Release links cleanly. The C build runs once and is
cached, so subsequent edits to the Rust code recompile fast.

## Release bundle

```bash
dx bundle --platform web --release
# produces ./dist/ ready to drop on any static host.
```

## Layout

The page is a single column with three stacked sections.

1. **Header**: project name + tagline + sample dropdown + Advanced
   options `<details>`. Picking a sample replaces the editor + the
   options that sample needs (PostGIS enables SQLiteGIS, RLS adds a
   session-variable mapping, etc.). The auto-translate watcher
   picks up the change and re-renders the right pane after the
   usual debounce.

2. **Two-column body**: PostgreSQL editor on the left, SQLite output
   on the right. Translation fires 700ms after the last edit. The
   left pane carries an inline error card when translation fails
   (with a Parser / Schema / Unsupported / ConfigRequired badge);
   the right pane has Copy + Download buttons that activate as soon
   as a translation succeeds. Below the SQLite view, a status line
   reports statement count + elapsed translation time, and an apply
   warning shows up if the translated SQL fails to execute against
   the in-page SQLite.

3. **Below**:
   - The **Query panel** appears once the translated schema has
     been applied to the in-memory SQLite. PG / SQLite dialect
     toggle (default: PG, with a "Translated as: ..." line showing
     what actually ran). Result rows render as a table; if the
     query result has `lon` / `lat` columns and SQLiteGIS is
     enabled, an equirectangular **PostGIS map** appears below the
     table with the points highlighted.
   - The **Reverse translation `<details>`** at the bottom takes a
     SQLite DML statement and produces the PG equivalent using the
     schema built from the live left-pane input.

## Status

In active development. See the parent repo's plan file for the
remaining work and the committed iteration history.
