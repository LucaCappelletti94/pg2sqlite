//! PL/pgSQL to `SQLite` translation module.
//!
//! This module handles the translation of PL/pgSQL trigger function bodies
//! to equivalent `SQLite` trigger statements using CTEs (Common Table
//! Expressions).
//!
//! ## Architecture
//!
//! The translation is split into several phases:
//!
//! 1. **Preprocessing** (`preprocessor.rs`): Transform PL/pgSQL-specific syntax
//!    (like `variable := expr`) into parseable SQL statements.
//!
//! 2. **Context Building** (`context.rs`): Extract variable declarations and
//!    build a context for tracking variable scope and types.
//!
//! 3. **CTE Building** (`cte_builder.rs`): Construct CTEs for variable bindings
//!    and shared expressions.
//!
//! 4. **Statement Translation** (`translator.rs`): Transform individual
//!    statements with proper CTE injection and condition handling.
//!
//! ## Translation Strategy
//!
//! PL/pgSQL functions use procedural constructs that don't exist in `SQLite`
//! triggers:
//!
//! - `DECLARE variable TYPE` → Tracked in context, no direct `SQLite`
//!   equivalent
//! - `variable := expr` → CTE with single value: `WITH var(val) AS (SELECT
//!   expr)`
//! - `IF condition THEN ... END IF` → `WHERE condition` clause on each
//!   statement
//! - Multiple statements sharing a variable → Each gets its own CTE (UUID
//!   regenerated)
//!
//! ### Example Translation
//!
//! **PL/pgSQL Input:**
//! ```sql
//! IF NOT EXISTS (SELECT 1 FROM t WHERE col = NEW.col) THEN
//!     v_id := uuidv7();
//!     INSERT INTO t (id, col) VALUES (v_id, NEW.col);
//! END IF;
//! ```
//!
//! **`SQLite` Output:**
//! ```sql
//! WITH v_id(val) AS (SELECT uuidv7())
//! INSERT INTO t (id, col)
//! SELECT v_id.val, NEW.col
//! FROM v_id
//! WHERE NOT EXISTS (SELECT 1 FROM t WHERE col = NEW.col);
//! ```

mod context;
mod cte_builder;
mod preprocessor;
mod translator;

pub use context::PlPgSqlContext;
pub use cte_builder::CteBuilder;
pub use preprocessor::PlPgSqlPreprocessor;
pub use translator::PlPgSqlTranslator;
