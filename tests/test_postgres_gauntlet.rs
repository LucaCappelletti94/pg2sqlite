//! Everything this suite checks against a real PostgreSQL, in one binary.
//!
//! One binary rather than five, because each one would start a container of its
//! own and the whole gauntlet costs less than a single start repeated. Each
//! gauntlet keeps its own module so the files stay separable.

mod helpers;

#[path = "helpers/postgres.rs"]
mod postgres_harness;

#[path = "gauntlet/source.rs"]
mod source;

#[path = "gauntlet/harness.rs"]
mod harness;

#[path = "gauntlet/reverse.rs"]
mod reverse;

#[path = "gauntlet/parity_rls.rs"]
mod parity_rls;

#[path = "gauntlet/parity_grants.rs"]
mod parity_grants;

#[path = "gauntlet/parity_plpgsql.rs"]
mod parity_plpgsql;

#[path = "gauntlet/parity_text.rs"]
mod parity_text;

#[path = "gauntlet/parity_datetime.rs"]
mod parity_datetime;

#[path = "gauntlet/parity_ddl.rs"]
mod parity_ddl;

#[path = "gauntlet/parity_dml.rs"]
mod parity_dml;

#[path = "gauntlet/parity_values.rs"]
mod parity_values;

#[path = "gauntlet/parity_numeric.rs"]
mod parity_numeric;
