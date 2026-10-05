//! The shared PostgreSQL harness works, so the gauntlets built on it start
//! from a known-good base.

use postgres_harness::{FIXTURES, apply, fixture_source, fresh_database, read_every_table};

use crate::postgres_harness;

/// One fixture that needs a session variable, one that needs a role the
/// prelude creates, and one that composes on another. Between them they cover
/// everything the harness has to supply.
#[test]
fn the_harness_supplies_what_the_fixtures_assume() {
    for name in ["rls_basic.sql", "rls_all_policy.sql", "rls_grants.sql"] {
        let fixture = FIXTURES
            .iter()
            .find(|candidate| candidate.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the inventory"));

        let mut connection = fresh_database();
        apply(&mut connection, &fixture_source(fixture))
            .unwrap_or_else(|error| panic!("{name} did not apply: {error}"));
        read_every_table(&mut connection, "app")
            .unwrap_or_else(|error| panic!("{name} applied but could not be read: {error}"));
    }
}
