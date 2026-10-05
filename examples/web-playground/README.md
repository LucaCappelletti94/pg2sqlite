# pg2sqlite live demo and project site

A Dioxus + sqlite-wasm-rs web app that runs entirely in the browser. Paste PostgreSQL DDL on the left, see the SQLite translation on the right (updates 700 ms after you stop typing), execute it against an in-memory SQLite, and run queries against the populated schema in either PG or SQLite dialect.

## Run it

```bash
dx serve --platform web --release
```

Visit `http://localhost:8080`. Use `--release` because `dioxus-code-editor` pulls in `arborium-tree-sitter`, whose C grammars fail to link under the `dev` profile (undefined `stderr`). The C build runs once and is cached, so subsequent Rust edits stay fast.

## Release bundle

```bash
dx bundle --platform web --release
# produces ./dist/ ready to drop on any static host.
```

## Deployment

The app is deployed to [`https://pg2sqlite.luca.phd`](https://pg2sqlite.luca.phd) by `.github/workflows/pages.yml` on every push to `main`. The workflow pins `dioxus-cli` to `0.7.9` to match the `dioxus` version in `Cargo.lock`. The custom domain is configured under repo Settings > Pages and the workflow writes a matching `CNAME` file into the build output.
