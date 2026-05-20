# Making `sqlparser-rs`'s `visitor` feature `no_std`-compatible

## TL;DR

`sqlparser-rs`'s crate root sets `#![cfg_attr(not(feature = "std"), no_std)]`
and the `visitor` feature is gated only behind `sqlparser_derive` (a
proc-macro crate). Despite that, **building `sqlparser` with
`--no-default-features --features visitor` fails for any target lacking
`std`** (e.g. `wasm32-unknown-unknown`, embedded targets) for two
reasons that both need fixing:

1. The generated `Visit` / `VisitMut` impls in `sqlparser_derive`
   hard-code `::std::ops::ControlFlow`.
2. `sqlparser/src/ast/visitor.rs` references `Vec` and `Box` without
   importing them from `alloc` under `no_std`.

Both fixes are mechanical and `no_std`-aware in a way that keeps the
`std` build path completely unchanged. A CI job that compiles the
public crate for `wasm32-unknown-unknown --no-default-features
--features visitor` prevents the same regression from landing again.

`core::ops::ControlFlow` has been stable since **Rust 1.55** (released
2021-09-09), so the path swap is MSRV-safe.

## Problem

`sqlparser`'s `Cargo.toml` exposes a `visitor` feature that depends only on
the `sqlparser_derive` proc-macro crate:

```toml
[features]
default = ["std", "recursive-protection"]
std = []
visitor = ["sqlparser_derive"]
recursive-protection = ["std", "recursive"]
```

A library consumer that wants `Visit` / `VisitMut` derives on a `no_std`
target therefore expects this combination to work:

```toml
[dependencies]
sqlparser = { version = "0.62", default-features = false, features = ["visitor"] }
```

Reality, as of sqlparser 0.62.0 and sqlparser_derive 0.5.0:

```text
$ cargo +stable check --target wasm32-unknown-unknown
   Compiling sqlparser v0.62.0
error[E0433]: cannot find `std` in the crate root
  --> .../sqlparser-0.62.0/src/ast/data_type.rs:34:40
   |
34 | #[cfg_attr(feature = "visitor", derive(Visit, VisitMut))]
   |                                        ^^^^^ could not find `std`
   |                                              in the list of imported crates
...
error: could not compile `sqlparser` (lib) due to 3201 previous errors
```

The 3201 errors are all the same root cause, surfaced once per
`#[derive(Visit, VisitMut)]` site (sqlparser has hundreds of AST nodes,
and each emits two trait impls).

After patching the derive macro only, the error count drops to 1137,
with the residue coming from `Vec` / `Box` resolution in
`src/ast/visitor.rs`.

## Root cause(s)

### Cause 1: `sqlparser_derive` emits `::std::ops::ControlFlow`

`sqlparser_derive` 0.5.0 emits the `visit` method body with an absolute
path through `::std`:

```rust
// sqlparser_derive-0.5.0/derive/src/visit.rs L56-L75
let expanded = quote! {
    impl #impl_generics sqlparser::ast::#visit_trait for #name #ty_generics #where_clause {
         #[cfg_attr(feature = "recursive-protection", recursive::recursive)]
        fn visit<V: sqlparser::ast::#visitor_trait>(
            &#modifier self,
            visitor: &mut V
        ) -> ::std::ops::ControlFlow<V::Break> {     // L65
            #pre_visit
            #children
            #post_visit
            ::std::ops::ControlFlow::Continue(())    // L69
        }
    }
};
```

When a downstream crate enables `sqlparser/visitor` without
`sqlparser/std`, the compiler runs in `no_std` mode and there is no
`std` crate in scope, so `::std::...` paths fail to resolve.

`ControlFlow` is the only `std` item the macro references; the rest
flows through `sqlparser::ast::*` or `recursive::recursive`. There
are no other `::std::` paths in `sqlparser_derive/src/`:

```bash
$ grep -rn "::std" sqlparser_derive-0.5.0/src/
sqlparser_derive-0.5.0/src/visit.rs:65: ) -> ::std::ops::ControlFlow<V::Break> {
sqlparser_derive-0.5.0/src/visit.rs:69:     ::std::ops::ControlFlow::Continue(())
```

### Cause 2: `sqlparser/src/ast/visitor.rs` uses bare `Vec` and `Box`

```rust
// sqlparser/src/ast/visitor.rs L62-L110 (excerpt)
impl<T: Visit> Visit for Option<T> { ... }
impl<T: Visit> Visit for Vec<T> { ... }   // L71
impl<T: Visit> Visit for Box<T> { ... }   // L80
impl<T: VisitMut> VisitMut for Option<T> { ... }
impl<T: VisitMut> VisitMut for Vec<T> { ... }   // L95
impl<T: VisitMut> VisitMut for Box<T> { ... }   // L104
```

Under `std`, the prelude provides `Vec` and `Box` automatically.
Under `no_std`, both live in `alloc` and must be imported explicitly.
The crate root has `extern crate alloc;` already, so the import is the
only thing missing.

## Fix

### `derive/src/visit.rs`

```diff
--- a/derive/src/visit.rs
+++ b/derive/src/visit.rs
@@ -62,12 +62,12 @@ pub(crate) fn derive_visit(
              #[cfg_attr(feature = "recursive-protection", recursive::recursive)]
             fn visit<V: sqlparser::ast::#visitor_trait>(
                 &#modifier self,
                 visitor: &mut V
-            ) -> ::std::ops::ControlFlow<V::Break> {
+            ) -> ::core::ops::ControlFlow<V::Break> {
                 #pre_visit
                 #children
                 #post_visit
-                ::std::ops::ControlFlow::Continue(())
+                ::core::ops::ControlFlow::Continue(())
             }
         }
     };
```

### `src/ast/visitor.rs`

```diff
--- a/src/ast/visitor.rs
+++ b/src/ast/visitor.rs
@@ -17,6 +17,9 @@
 
 //! Recursive visitors for ast Nodes. See [`Visitor`] for more details.
 
+#[cfg(not(feature = "std"))]
+use alloc::{boxed::Box, string::String, vec::Vec};
+
 use crate::ast::{Expr, ObjectName, Query, Select, Statement, TableFactor, ValueWithSpan};
 use core::ops::ControlFlow;
```

(`String` is included because the blanket impl set covers it too, even
though that specific impl line is the same as for `Vec` — keeping the
import set tight to what the file actually references avoids a
`dead_code` warning on the `String` import only when no `String` impl
exists, so audit and trim before committing.)

### Why this is safe

- `core::ops::ControlFlow` has been stable since Rust 1.55 (2021-09-09).
  `std::ops::ControlFlow` is itself just a re-export of the `core` type,
  so the change is purely path normalization — the underlying type is
  identical, including its inherent methods and `Continue` / `Break`
  variants.
- `core` is always in scope in both `std` and `no_std` builds; `std` is
  not available under `no_std`. Using `::core::...` is therefore strictly
  more portable.
- `alloc::{Box, String, Vec}` resolve identically to the `std` prelude
  re-exports they replace; importing them via `#[cfg(not(feature =
  "std"))]` means `std` builds are completely unaffected.
- No public API change. Downstream code that imports
  `std::ops::ControlFlow` or `std::vec::Vec` continues to compile.

## Test plan

The bug is silent: nothing in CI exercises the `visitor` feature in a
`no_std` context, which is why it has persisted across releases. Two
complementary checks plug that gap.

### 1. Per-crate compile-only check (cheapest signal)

Add a CI job that compiles the public `sqlparser` crate for
`wasm32-unknown-unknown` with the `visitor` feature but without `std`:

```yaml
# .github/workflows/no_std.yml (or merged into existing CI)
name: no_std + WASM
on: [push, pull_request]
jobs:
  wasm_visitor:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown
      - name: cargo check (no_std + visitor)
        run: |
          cargo check -p sqlparser \
            --target wasm32-unknown-unknown \
            --no-default-features \
            --features visitor
```

This single `cargo check` exercises every `#[derive(Visit, VisitMut)]`
expansion against the proc-macro output. With both fixes applied it
completes in under a minute on a cold target install; without the fix
it emits thousands of errors immediately.

### 2. Regression fixture in `sqlparser_derive`

Add an integration test that depends on `sqlparser` with `visitor`
under `#![no_std]`, asserting the generated impl path compiles. The
fixture does not need to run — it only needs to type-check:

```rust
// sqlparser/tests/no_std_visitor.rs (compile-only fixture)
#![no_std]

extern crate alloc;

use sqlparser::ast::{Statement, Visit};

fn _assert_visit_compiles<T: Visit>() {}

fn _probe() {
    _assert_visit_compiles::<Statement>();
}
```

Wire it via a `[[test]]` entry that is built but not run, or set it up
as a workspace member that the CI step in (1) consumes.

### 3. Manual reproduction (for the PR description)

```bash
rustup target add wasm32-unknown-unknown
git clone https://github.com/apache/datafusion-sqlparser-rs
cd datafusion-sqlparser-rs
# Apply the diff from this document (or `git fetch` the PR branch).

# Before the fix:
cargo check -p sqlparser --target wasm32-unknown-unknown \
  --no-default-features --features visitor   # ~3201 errors

# After the derive-only fix:
cargo check -p sqlparser --target wasm32-unknown-unknown \
  --no-default-features --features visitor   # ~1137 errors

# After both fixes:
cargo check -p sqlparser --target wasm32-unknown-unknown \
  --no-default-features --features visitor   # clean
```

The diffs have zero effect on `std` builds because `std::ops::ControlFlow
== core::ops::ControlFlow` at the type level and the `alloc::*` imports
are gated `#[cfg(not(feature = "std"))]`.

## Downstream impact

This unblocks several `no_std` / WASM goals:

- `sql-traits` (`earth-metabolome-initiative/sql-traits`) — HEAD already
  ships a `default = ["std"]` feature split that conditionally enables
  `sqlparser/std` and `sqlparser/recursive-protection`. Today its
  `no_std` build path avoids `visitor`, which forces downstream crates
  to also avoid `Visit` / `VisitMut`. With the fix, sql-traits can
  forward `sqlparser/visitor` unconditionally.
- `pg2sqlite` (`LucaCappelletti94/pg2sqlite`) — currently has exactly
  one Visit consumer (`src/impls/reverse_translator_impls/statement.rs`,
  the RLS-aware reverse-translation walker). With the fix landed,
  pg2sqlite can compile for `wasm32-unknown-unknown` for in-browser
  SQL translation without rewriting that walker.
- Any embedded user wanting AST traversal under `no_std`.

## Backporting / patch table workaround

While the upstream PR is in review, downstream crates can pin a forked
sqlparser + sqlparser_derive via `[patch.crates-io]`:

```toml
[patch.crates-io]
sqlparser = { git = "https://github.com/<user>/sqlparser-rs", rev = "<rev>" }
sqlparser_derive = { git = "https://github.com/<user>/sqlparser-rs", rev = "<rev>" }
```

Both crates need patching because the visitor.rs alloc imports live in
`sqlparser`, while the `ControlFlow` path lives in `sqlparser_derive`.
Once the upstream PR merges and patched releases ship (e.g.
sqlparser 0.63 + sqlparser_derive 0.5.1), the patch table entry can
be deleted.

## Checklist for the upstream PR

- [ ] `derive/src/visit.rs`: swap both `::std` references for `::core`.
- [ ] `src/ast/visitor.rs`: add `#[cfg(not(feature = "std"))] use
      alloc::{boxed::Box, string::String, vec::Vec};` (audit `String`
      need).
- [ ] Add `#![no_std]` compile-test fixture (Test plan §2) so the same
      regression cannot reappear.
- [ ] Add CI job for `cargo check -p sqlparser --target
      wasm32-unknown-unknown --no-default-features --features visitor`
      (Test plan §1).
- [ ] CHANGELOG entry: "The `sqlparser/visitor` feature now compiles
      under `no_std`. `Visit` / `VisitMut` derives route `ControlFlow`
      through `core::ops`; `alloc::{Box, String, Vec}` are imported in
      `visitor.rs` under `#[cfg(not(feature = "std"))]`."
- [ ] Patch-release sqlparser_derive (no API change; safe minor bump).
