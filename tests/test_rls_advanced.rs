//! Advanced RLS translation tests targeting uncovered paths in
//! `src/impls/translator_impls/rls.rs`.
//!
//! Covers: EXISTS in policy USING, Subquery in policy, UPDATE with CHECK,
//! CompoundIdentifier rename in trigger, find_current_setting_call in BinaryOp,
//! contains_current_user in nested expr, validate_session_variables error,
//! generate_readonly_rls_statements, monitoring triggers, validation views,
//! generate_rls_audit_table, InList/Between/IsNull in transform_expr,
//! InList/Between in transform_outer_table_refs.

use pg2sqlite::prelude::{Pg2Sqlite, Pg2SqliteOptions, SessionVariableMapping};

fn make_options() -> Pg2SqliteOptions {
    Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ))
}

fn translate(sql: &str) -> String {
    let options = make_options();
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(&options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_with_options(sql: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(sql)
        .unwrap()
        .translate(options)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `INSTEAD OF UPDATE` trigger out of a translated script.
///
/// An UPDATE names a row that already exists and SQLite fills `NEW` from it, so
/// nothing in that trigger may fall back to another value. The INSERT trigger
/// is the opposite case, where a column the caller omitted has to pick up its
/// declared default, so the assertion has to name the trigger it means.
fn update_trigger(output: &str) -> &str {
    output
        .lines()
        .find(|line| line.contains("INSTEAD OF UPDATE"))
        .unwrap_or_else(|| panic!("no INSTEAD OF UPDATE trigger in:\n{output}"))
}

/// Executes every translated statement in an in-memory SQLite connection.
/// Uses the standard RLS options from make_options().
fn execute_rls_ddl(pg_sql: &str) {
    let stmts = Pg2Sqlite::default().sql(pg_sql).unwrap().translate(&make_options()).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};")).expect("translated RLS DDL must execute in SQLite");
    }
}

/// Like execute_rls_ddl but uses caller-supplied options.
fn execute_rls_ddl_with_opts(pg_sql: &str, options: &Pg2SqliteOptions) {
    let stmts = Pg2Sqlite::default().sql(pg_sql).unwrap().translate(options).unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for stmt in &stmts {
        conn.execute_batch(&format!("{stmt};")).expect("translated RLS DDL must execute in SQLite");
    }
}

#[test]
fn exists_in_policy_using() {
    let sql = r#"
        CREATE TABLE orgs (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE documents (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            title TEXT NOT NULL
        );
        ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON documents
            FOR SELECT TO authenticated
            USING (EXISTS (SELECT 1 FROM orgs WHERE orgs.id = documents.org_id));
        CREATE POLICY docs_insert ON documents
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY docs_update ON documents
            FOR UPDATE TO authenticated
            USING (EXISTS (SELECT 1 FROM orgs WHERE orgs.id = documents.org_id))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY docs_delete ON documents
            FOR DELETE TO authenticated
            USING (EXISTS (SELECT 1 FROM orgs WHERE orgs.id = documents.org_id));
    "#;
    let output = translate(sql);
    assert!(output.contains("EXISTS"), "Expected EXISTS in view: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn in_subquery_in_policy() {
    let sql = r#"
        CREATE TABLE departments (id INT PRIMARY KEY, name TEXT);
        CREATE TABLE employees (
            id INT PRIMARY KEY,
            department_id INT NOT NULL,
            name TEXT NOT NULL
        );
        ALTER TABLE employees ENABLE ROW LEVEL SECURITY;
        CREATE POLICY emp_select ON employees
            FOR SELECT TO authenticated
            USING (department_id IN (SELECT id FROM departments));
        CREATE POLICY emp_insert ON employees
            FOR INSERT TO authenticated
            WITH CHECK (department_id IN (SELECT id FROM departments));
        CREATE POLICY emp_update ON employees
            FOR UPDATE TO authenticated
            USING (department_id IN (SELECT id FROM departments))
            WITH CHECK (department_id IN (SELECT id FROM departments));
        CREATE POLICY emp_delete ON employees
            FOR DELETE TO authenticated
            USING (department_id IN (SELECT id FROM departments));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_with_check_resolves_against_new_row() {
    let sql = r#"
        CREATE TABLE items (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY items_select ON items
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY items_insert ON items
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY items_update ON items
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY items_delete ON items
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // WITH CHECK constrains the NEW row, so its column refs resolve against NEW,
    // and the forwarding SET clause assigns NEW.col directly. Neither uses
    // COALESCE, which used to make `SET owner_id = NULL` a silent no-op.
    assert!(output.contains("NEW.owner_id"), "WITH CHECK must reference NEW: {output}");
    assert!(output.contains("owner_id = NEW.owner_id"), "SET must assign NEW: {output}");
    assert!(
        !update_trigger(&output).contains("COALESCE"),
        "the SET clause must assign NEW.col directly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn compound_identifier_renamed_in_trigger() {
    let sql = r#"
        CREATE TABLE docs (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY docs_select ON docs
            FOR SELECT TO authenticated
            USING (docs.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY docs_insert ON docs
            FOR INSERT TO authenticated
            WITH CHECK (docs.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY docs_update ON docs
            FOR UPDATE TO authenticated
            USING (docs.owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (docs.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY docs_delete ON docs
            FOR DELETE TO authenticated
            USING (docs.owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // In trigger context, docs.owner_id should become NEW.owner_id or OLD.owner_id
    assert!(
        output.contains("NEW.owner_id") || output.contains("OLD.owner_id"),
        "Expected NEW/OLD prefix for compound identifier: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn current_setting_in_binary_op() {
    let sql = r#"
        CREATE TABLE tenants (
            id INT PRIMARY KEY,
            tenant_id INT NOT NULL
        );
        ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
        CREATE POLICY tenant_select ON tenants
            FOR SELECT TO authenticated
            USING (current_setting('app.user_id')::int = tenant_id);
        CREATE POLICY tenant_insert ON tenants
            FOR INSERT TO authenticated
            WITH CHECK (current_setting('app.user_id')::int = tenant_id);
        CREATE POLICY tenant_update ON tenants
            FOR UPDATE TO authenticated
            USING (current_setting('app.user_id')::int = tenant_id)
            WITH CHECK (current_setting('app.user_id')::int = tenant_id);
        CREATE POLICY tenant_delete ON tenants
            FOR DELETE TO authenticated
            USING (current_setting('app.user_id')::int = tenant_id);
    "#;
    let output = translate(sql);
    assert!(
        output.contains("current_app_user()"),
        "Expected current_app_user() function: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn current_user_in_nested_expr() {
    let sql = r#"
        CREATE TABLE resources (
            id INT PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE resources ENABLE ROW LEVEL SECURITY;
        CREATE POLICY res_select ON resources
            FOR SELECT TO authenticated
            USING ((current_user = owner));
        CREATE POLICY res_insert ON resources
            FOR INSERT TO authenticated
            WITH CHECK ((current_user = owner));
        CREATE POLICY res_update ON resources
            FOR UPDATE TO authenticated
            USING ((current_user = owner))
            WITH CHECK ((current_user = owner));
        CREATE POLICY res_delete ON resources
            FOR DELETE TO authenticated
            USING ((current_user = owner));
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_user("current_app_username"));
    let output = translate_with_options(sql, &options);
    assert!(output.contains("current_app_username()"), "Expected current_app_username(): {output}");
    execute_rls_ddl_with_opts(sql, &options);
}

#[test]
fn missing_session_variable_mapping_error() {
    let sql = r#"
        CREATE TABLE secrets (
            id INT PRIMARY KEY,
            org_id TEXT NOT NULL
        );
        ALTER TABLE secrets ENABLE ROW LEVEL SECURITY;
        CREATE POLICY secrets_select ON secrets
            FOR SELECT TO authenticated
            USING (org_id = current_setting('app.org_id')::text);
    "#;
    // Options that don't include mapping for 'app.org_id'
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string());

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);
    assert!(result.is_err(), "Expected error for missing mapping");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("session variable mapping"),
        "Expected session variable mapping error: {err}"
    );
}

#[test]
fn missing_one_of_multiple_current_setting_mappings_errors() {
    let sql = r#"
        CREATE TABLE scoped_secrets (
            id INT PRIMARY KEY,
            user_id TEXT NOT NULL,
            org_id TEXT NOT NULL
        );
        ALTER TABLE scoped_secrets ENABLE ROW LEVEL SECURITY;
        CREATE POLICY scoped_secrets_select ON scoped_secrets
            FOR SELECT TO authenticated
            USING (
                user_id = current_setting('app.user_id')::text
                OR org_id = current_setting('app.org_id')::text
            );
    "#;
    // Only app.user_id is mapped; app.org_id is missing and should error.
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);
    assert!(
        result.is_err(),
        "Expected missing mapping error when one of multiple current_setting calls is unmapped"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("app.org_id"),
        "Expected error to reference missing app.org_id mapping: {err}"
    );
}

#[test]
fn readonly_rls_table_no_write_triggers() {
    let sql = r#"
        CREATE ROLE authenticated;
        CREATE TABLE readonly_docs (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL,
            title TEXT NOT NULL
        );
        ALTER TABLE readonly_docs ENABLE ROW LEVEL SECURITY;
        GRANT SELECT ON readonly_docs TO authenticated;
        CREATE POLICY readonly_docs_select ON readonly_docs
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // Should have a view but NOT insert/update/delete triggers
    assert!(
        output.contains("CREATE VIEW readonly_docs"),
        "Expected view for readonly table: {output}"
    );
    assert!(
        !output.contains("insert_trigger"),
        "Should NOT have insert trigger for readonly: {output}"
    );
    assert!(
        !output.contains("update_trigger"),
        "Should NOT have update trigger for readonly: {output}"
    );
    assert!(
        !output.contains("delete_trigger"),
        "Should NOT have delete trigger for readonly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn monitoring_triggers_in_monitor_mode() {
    let sql = r#"
        CREATE TABLE monitored (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE monitored ENABLE ROW LEVEL SECURITY;
        CREATE POLICY mon_select ON monitored
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY mon_insert ON monitored
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY mon_update ON monitored
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY mon_delete ON monitored
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // Should have monitoring triggers
    assert!(output.contains("rls_monitor_insert"), "Expected monitor insert trigger: {output}");
    assert!(output.contains("rls_monitor_update"), "Expected monitor update trigger: {output}");
    // In monitor mode (default), should have severity 'warning'
    assert!(output.contains("warning"), "Expected 'warning' severity in monitor mode: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn monitoring_triggers_in_strict_mode() {
    let sql = r#"
        CREATE TABLE strict_table (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE strict_table ENABLE ROW LEVEL SECURITY;
        CREATE POLICY strict_select ON strict_table
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY strict_insert ON strict_table
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY strict_update ON strict_table
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY strict_delete ON strict_table
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let options = make_options().with_strict_rls_validation();
    let output = translate_with_options(sql, &options);
    // Strict mode should contain RAISE(ABORT)
    assert!(output.contains("RAISE(ABORT"), "Expected RAISE(ABORT) in strict mode: {output}");
    // In strict mode, severity should be 'error'
    assert!(output.contains("error"), "Expected 'error' severity in strict mode: {output}");
    execute_rls_ddl_with_opts(sql, &options);
}

#[test]
fn validation_view_generated() {
    let sql = r#"
        CREATE TABLE validated (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE validated ENABLE ROW LEVEL SECURITY;
        CREATE POLICY val_select ON validated
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY val_insert ON validated
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY val_update ON validated
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY val_delete ON validated
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("_violations"), "Expected validation view with _violations: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn audit_table_generated() {
    let sql = r#"
        CREATE TABLE audited (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE audited ENABLE ROW LEVEL SECURITY;
        CREATE POLICY aud_select ON audited
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY aud_insert ON audited
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY aud_update ON audited
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY aud_delete ON audited
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("CREATE TABLE IF NOT EXISTS rls_audit"),
        "Expected audit table creation: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn is_null_in_policy() {
    let sql = r#"
        CREATE TABLE nullable_docs (
            id INT PRIMARY KEY,
            deleted_at TEXT,
            owner_id INT NOT NULL
        );
        ALTER TABLE nullable_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY null_select ON nullable_docs
            FOR SELECT TO authenticated
            USING (deleted_at IS NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY null_insert ON nullable_docs
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY null_update ON nullable_docs
            FOR UPDATE TO authenticated
            USING (deleted_at IS NULL AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY null_delete ON nullable_docs
            FOR DELETE TO authenticated
            USING (deleted_at IS NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NULL"), "Expected IS NULL in transformed policy: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn in_list_in_policy() {
    let sql = r#"
        CREATE TABLE role_items (
            id INT PRIMARY KEY,
            role TEXT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE role_items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY role_select ON role_items
            FOR SELECT TO authenticated
            USING (role IN ('admin', 'owner') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY role_insert ON role_items
            FOR INSERT TO authenticated
            WITH CHECK (role IN ('admin', 'owner') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY role_update ON role_items
            FOR UPDATE TO authenticated
            USING (role IN ('admin', 'owner') AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (role IN ('admin', 'owner') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY role_delete ON role_items
            FOR DELETE TO authenticated
            USING (role IN ('admin', 'owner') AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN ("), "Expected IN list in transformed policy: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn between_in_policy() {
    let sql = r#"
        CREATE TABLE age_restricted (
            id INT PRIMARY KEY,
            age INT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE age_restricted ENABLE ROW LEVEL SECURITY;
        CREATE POLICY age_select ON age_restricted
            FOR SELECT TO authenticated
            USING (age BETWEEN 18 AND 65 AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY age_insert ON age_restricted
            FOR INSERT TO authenticated
            WITH CHECK (age BETWEEN 18 AND 65 AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY age_update ON age_restricted
            FOR UPDATE TO authenticated
            USING (age BETWEEN 18 AND 65 AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (age BETWEEN 18 AND 65 AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY age_delete ON age_restricted
            FOR DELETE TO authenticated
            USING (age BETWEEN 18 AND 65 AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("BETWEEN"), "Expected BETWEEN in transformed policy: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn missing_audit_table_name_error() {
    let sql = r#"
        CREATE TABLE no_audit (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE no_audit ENABLE ROW LEVEL SECURITY;
        CREATE POLICY no_aud_select ON no_audit
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_session_variable(SessionVariableMapping::current_setting(
            "app.user_id",
            "current_app_user",
        ));
    // No audit table name configured
    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);
    assert!(result.is_err(), "Expected error for missing audit table name");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("audit table name"), "Expected audit table name error: {err}");
}

#[test]
fn is_not_null_in_policy() {
    let sql = r#"
        CREATE TABLE verified (
            id INT PRIMARY KEY,
            verified_at TEXT,
            owner_id INT NOT NULL
        );
        ALTER TABLE verified ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ver_select ON verified
            FOR SELECT TO authenticated
            USING (verified_at IS NOT NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ver_insert ON verified
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ver_update ON verified
            FOR UPDATE TO authenticated
            USING (verified_at IS NOT NULL AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ver_delete ON verified
            FOR DELETE TO authenticated
            USING (verified_at IS NOT NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NOT NULL"), "Expected IS NOT NULL in transformed policy: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn unary_not_in_policy() {
    let sql = r#"
        CREATE TABLE flagged (
            id INT PRIMARY KEY,
            is_deleted INT NOT NULL DEFAULT 0,
            owner_id INT NOT NULL
        );
        ALTER TABLE flagged ENABLE ROW LEVEL SECURITY;
        CREATE POLICY flag_select ON flagged
            FOR SELECT TO authenticated
            USING (NOT is_deleted AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY flag_insert ON flagged
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY flag_update ON flagged
            FOR UPDATE TO authenticated
            USING (NOT is_deleted AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY flag_delete ON flagged
            FOR DELETE TO authenticated
            USING (NOT is_deleted AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("NOT"), "Expected NOT in transformed policy: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn nested_parens_in_policy() {
    let sql = r#"
        CREATE TABLE nested_items (
            id INT PRIMARY KEY,
            category TEXT NOT NULL,
            status TEXT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE nested_items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY nested_select ON nested_items
            FOR SELECT TO authenticated
            USING ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY nested_insert ON nested_items
            FOR INSERT TO authenticated
            WITH CHECK ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY nested_update ON nested_items
            FOR UPDATE TO authenticated
            USING ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY nested_delete ON nested_items
            FOR DELETE TO authenticated
            USING ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("category") && output.contains("status"),
        "Expected nested conditions in policy: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_not_operator() {
    let sql = r#"
        CREATE TABLE check_not (
            id INT PRIMARY KEY,
            is_archived INT NOT NULL DEFAULT 0,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_not ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cn_select ON check_not
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cn_insert ON check_not
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cn_update ON check_not
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (NOT is_archived AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cn_delete ON check_not
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("NOT NEW.is_archived"), "NOT must apply to NEW: {output}");
    assert!(output.contains("NEW.owner_id"), "WITH CHECK must reference NEW: {output}");
    assert!(
        !update_trigger(&output).contains("COALESCE"),
        "the SET clause must assign NEW.col directly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_nested_expr() {
    let sql = r#"
        CREATE TABLE check_nested (
            id INT PRIMARY KEY,
            category TEXT NOT NULL,
            status TEXT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_nested ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cne_select ON check_nested
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cne_insert ON check_nested
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cne_update ON check_nested
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK ((category = 'A' OR status = 'active') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cne_delete ON check_nested
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("NEW.category"), "nested OR arm must reference NEW: {output}");
    assert!(output.contains("NEW.status"), "nested OR arm must reference NEW: {output}");
    assert!(output.contains("NEW.owner_id"), "WITH CHECK must reference NEW: {output}");
    assert!(
        !update_trigger(&output).contains("COALESCE"),
        "the SET clause must assign NEW.col directly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_is_null() {
    let sql = r#"
        CREATE TABLE check_null (
            id INT PRIMARY KEY,
            deleted_at TEXT,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_null ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cnl_select ON check_null
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnl_insert ON check_null
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnl_update ON check_null
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (deleted_at IS NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnl_delete ON check_null
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NULL"), "Expected IS NULL in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_is_not_null() {
    let sql = r#"
        CREATE TABLE check_not_null (
            id INT PRIMARY KEY,
            verified_at TEXT,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_not_null ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cnn_select ON check_not_null
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnn_insert ON check_not_null
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnn_update ON check_not_null
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (verified_at IS NOT NULL AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cnn_delete ON check_not_null
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NOT NULL"), "Expected IS NOT NULL in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_in_list() {
    let sql = r#"
        CREATE TABLE check_in_list (
            id INT PRIMARY KEY,
            role TEXT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_in_list ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cil_select ON check_in_list
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cil_insert ON check_in_list
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cil_update ON check_in_list
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (role IN ('admin', 'editor') AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cil_delete ON check_in_list
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN ("), "Expected IN list in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_exists_subquery() {
    let sql = r#"
        CREATE TABLE check_exists (
            id INT PRIMARY KEY,
            group_id INT NOT NULL,
            owner_id INT NOT NULL
        );
        CREATE TABLE allowed_groups (
            id INT PRIMARY KEY,
            user_id INT NOT NULL
        );
        ALTER TABLE check_exists ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ce_select ON check_exists
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ce_insert ON check_exists
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ce_update ON check_exists
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (EXISTS (SELECT 1 FROM allowed_groups WHERE allowed_groups.id = check_exists.group_id) AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ce_delete ON check_exists
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("EXISTS"), "Expected EXISTS in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_in_subquery() {
    let sql = r#"
        CREATE TABLE check_in_sub (
            id INT PRIMARY KEY,
            dept_id INT NOT NULL,
            owner_id INT NOT NULL
        );
        CREATE TABLE depts (
            id INT PRIMARY KEY,
            active INT NOT NULL
        );
        ALTER TABLE check_in_sub ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cis_select ON check_in_sub
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cis_insert ON check_in_sub
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cis_update ON check_in_sub
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (dept_id IN (SELECT id FROM depts WHERE active = 1) AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cis_delete ON check_in_sub
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN subquery in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_compound_identifier() {
    let sql = r#"
        CREATE TABLE check_compound (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_compound ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cc_select ON check_compound
            FOR SELECT TO authenticated
            USING (check_compound.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cc_insert ON check_compound
            FOR INSERT TO authenticated
            WITH CHECK (check_compound.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cc_update ON check_compound
            FOR UPDATE TO authenticated
            USING (check_compound.owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (check_compound.owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cc_delete ON check_compound
            FOR DELETE TO authenticated
            USING (check_compound.owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // The policy qualifies the column as `check_compound.owner_id`. In the WITH
    // CHECK context the prefix wins over the backing-table rename, so it becomes
    // NEW.owner_id rather than check_compound_rls.owner_id.
    assert!(output.contains("NEW.owner_id"), "compound ref must become NEW: {output}");
    assert!(
        !update_trigger(&output).contains("COALESCE"),
        "the SET clause must assign NEW.col directly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_is_null_in_subquery() {
    let sql = r#"
        CREATE TABLE docs_outer (
            id INT PRIMARY KEY,
            deleted_at TEXT,
            org_id INT NOT NULL
        );
        CREATE TABLE org_access (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE docs_outer ENABLE ROW LEVEL SECURITY;
        CREATE POLICY do_select ON docs_outer
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access
                WHERE org_access.org_id = docs_outer.org_id
                AND docs_outer.deleted_at IS NULL
                AND org_access.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY do_insert ON docs_outer
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY do_update ON docs_outer
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access
                WHERE org_access.org_id = docs_outer.org_id
                AND docs_outer.deleted_at IS NULL
                AND org_access.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY do_delete ON docs_outer
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access
                WHERE org_access.org_id = docs_outer.org_id
                AND docs_outer.deleted_at IS NULL
                AND org_access.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NULL"), "Expected IS NULL in subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_in_list_in_subquery() {
    let sql = r#"
        CREATE TABLE tagged_docs (
            id INT PRIMARY KEY,
            tag TEXT NOT NULL,
            org_id INT NOT NULL
        );
        CREATE TABLE org_access2 (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE tagged_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY td_select ON tagged_docs
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access2
                WHERE org_access2.org_id = tagged_docs.org_id
                AND tagged_docs.tag IN ('public', 'shared')
                AND org_access2.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY td_insert ON tagged_docs
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY td_update ON tagged_docs
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access2
                WHERE org_access2.org_id = tagged_docs.org_id
                AND tagged_docs.tag IN ('public', 'shared')
                AND org_access2.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY td_delete ON tagged_docs
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access2
                WHERE org_access2.org_id = tagged_docs.org_id
                AND tagged_docs.tag IN ('public', 'shared')
                AND org_access2.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN ("), "Expected IN list in subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_nested_in_subquery() {
    let sql = r#"
        CREATE TABLE nested_docs (
            id INT PRIMARY KEY,
            status TEXT NOT NULL,
            priority INT NOT NULL,
            org_id INT NOT NULL
        );
        CREATE TABLE org_members (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE nested_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY nd_select ON nested_docs
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_members
                WHERE org_members.org_id = nested_docs.org_id
                AND (nested_docs.status = 'active' OR nested_docs.priority > 5)
                AND org_members.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY nd_insert ON nested_docs
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY nd_update ON nested_docs
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_members
                WHERE org_members.org_id = nested_docs.org_id
                AND (nested_docs.status = 'active' OR nested_docs.priority > 5)
                AND org_members.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY nd_delete ON nested_docs
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_members
                WHERE org_members.org_id = nested_docs.org_id
                AND (nested_docs.status = 'active' OR nested_docs.priority > 5)
                AND org_members.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("status") && output.contains("priority"),
        "Expected nested expr in subquery: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn for_all_policy() {
    let sql = r#"
        CREATE TABLE simple_rls (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE simple_rls ENABLE ROW LEVEL SECURITY;
        CREATE POLICY all_policy ON simple_rls
            FOR ALL TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("CREATE VIEW simple_rls"),
        "Expected RLS view for FOR ALL policy: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_between() {
    let sql = r#"
        CREATE TABLE check_between (
            id INT PRIMARY KEY,
            priority INT NOT NULL,
            owner_id INT NOT NULL
        );
        ALTER TABLE check_between ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cb_select ON check_between
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cb_insert ON check_between
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cb_update ON check_between
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (priority BETWEEN 1 AND 10 AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cb_delete ON check_between
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("BETWEEN"), "Expected BETWEEN in update check: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn update_check_with_subquery() {
    let sql = r#"
        CREATE TABLE check_subq (
            id INT PRIMARY KEY,
            max_allowed INT NOT NULL,
            owner_id INT NOT NULL
        );
        CREATE TABLE limits (
            id INT PRIMARY KEY,
            max_val INT NOT NULL
        );
        ALTER TABLE check_subq ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cs_select ON check_subq
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cs_insert ON check_subq
            FOR INSERT TO authenticated
            WITH CHECK (owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cs_update ON check_subq
            FOR UPDATE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT))
            WITH CHECK (max_allowed <= (SELECT MAX(max_val) FROM limits) AND owner_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY cs_delete ON check_subq
            FOR DELETE TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    assert!(output.contains("NEW.max_allowed"), "WITH CHECK must reference NEW: {output}");
    assert!(output.contains("NEW.owner_id"), "WITH CHECK must reference NEW: {output}");
    assert!(output.contains("SELECT MAX(max_val) FROM limits"), "subquery preserved: {output}");
    assert!(
        !update_trigger(&output).contains("COALESCE"),
        "the SET clause must assign NEW.col directly: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn policy_with_cast_on_session_var() {
    let sql = r#"
        CREATE TABLE cast_docs (
            id INT PRIMARY KEY,
            tenant_id TEXT NOT NULL
        );
        ALTER TABLE cast_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY cd_select ON cast_docs
            FOR SELECT TO authenticated
            USING (tenant_id = current_setting('app.user_id')::text);
        CREATE POLICY cd_insert ON cast_docs
            FOR INSERT TO authenticated
            WITH CHECK (tenant_id = current_setting('app.user_id')::text);
        CREATE POLICY cd_update ON cast_docs
            FOR UPDATE TO authenticated
            USING (tenant_id = current_setting('app.user_id')::text)
            WITH CHECK (tenant_id = current_setting('app.user_id')::text);
        CREATE POLICY cd_delete ON cast_docs
            FOR DELETE TO authenticated
            USING (tenant_id = current_setting('app.user_id')::text);
    "#;
    let output = translate(sql);
    assert!(
        output.contains("current_app_user()"),
        "Expected cast to be removed and function mapped: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn readonly_rls_with_monitoring_triggers() {
    let sql = r#"
        CREATE ROLE authenticated;
        CREATE TABLE ro_monitored (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL,
            data TEXT
        );
        ALTER TABLE ro_monitored ENABLE ROW LEVEL SECURITY;
        GRANT SELECT ON ro_monitored TO authenticated;
        CREATE POLICY ro_select ON ro_monitored
            FOR SELECT TO authenticated
            USING (owner_id = CAST(current_setting('app.user_id') AS INT));
    "#;
    let output = translate(sql);
    // Readonly still gets monitoring triggers
    assert!(
        output.contains("rls_monitor_insert"),
        "Expected monitoring triggers on readonly: {output}"
    );
    assert!(output.contains("_violations"), "Expected validation view on readonly: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn subquery_in_policy() {
    let sql = r#"
        CREATE TABLE access_controlled (
            id INT PRIMARY KEY,
            group_id INT NOT NULL
        );
        CREATE TABLE access_groups (
            id INT PRIMARY KEY,
            user_id INT NOT NULL
        );
        ALTER TABLE access_controlled ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ac_select ON access_controlled
            FOR SELECT TO authenticated
            USING (group_id = (SELECT id FROM access_groups WHERE user_id = CAST(current_setting('app.user_id') AS INT) LIMIT 1));
        CREATE POLICY ac_insert ON access_controlled
            FOR INSERT TO authenticated
            WITH CHECK (group_id = (SELECT id FROM access_groups WHERE user_id = CAST(current_setting('app.user_id') AS INT) LIMIT 1));
        CREATE POLICY ac_update ON access_controlled
            FOR UPDATE TO authenticated
            USING (group_id = (SELECT id FROM access_groups WHERE user_id = CAST(current_setting('app.user_id') AS INT) LIMIT 1))
            WITH CHECK (group_id = (SELECT id FROM access_groups WHERE user_id = CAST(current_setting('app.user_id') AS INT) LIMIT 1));
        CREATE POLICY ac_delete ON access_controlled
            FOR DELETE TO authenticated
            USING (group_id = (SELECT id FROM access_groups WHERE user_id = CAST(current_setting('app.user_id') AS INT) LIMIT 1));
    "#;
    let output = translate(sql);
    assert!(
        output.contains("SELECT") && output.contains("access_groups"),
        "Expected subquery referencing access_groups: {output}"
    );
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_is_not_null_in_subquery() {
    let sql = r#"
        CREATE TABLE active_docs (
            id INT PRIMARY KEY,
            published_at TEXT,
            org_id INT NOT NULL
        );
        CREATE TABLE org_access3 (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE active_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ad_select ON active_docs
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access3
                WHERE org_access3.org_id = active_docs.org_id
                AND active_docs.published_at IS NOT NULL
                AND org_access3.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY ad_insert ON active_docs
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ad_update ON active_docs
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access3
                WHERE org_access3.org_id = active_docs.org_id
                AND active_docs.published_at IS NOT NULL
                AND org_access3.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY ad_delete ON active_docs
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access3
                WHERE org_access3.org_id = active_docs.org_id
                AND active_docs.published_at IS NOT NULL
                AND org_access3.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(output.contains("IS NOT NULL"), "Expected IS NOT NULL in subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_between_in_subquery() {
    let sql = r#"
        CREATE TABLE scored_items (
            id INT PRIMARY KEY,
            score INT NOT NULL,
            min_score INT NOT NULL,
            max_score INT NOT NULL,
            org_id INT NOT NULL
        );
        CREATE TABLE org_scores (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE scored_items ENABLE ROW LEVEL SECURITY;
        CREATE POLICY si_select ON scored_items
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_scores
                WHERE org_scores.org_id = scored_items.org_id
                AND scored_items.score BETWEEN scored_items.min_score AND scored_items.max_score
                AND org_scores.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY si_insert ON scored_items
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY si_update ON scored_items
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_scores
                WHERE org_scores.org_id = scored_items.org_id
                AND scored_items.score BETWEEN scored_items.min_score AND scored_items.max_score
                AND org_scores.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY si_delete ON scored_items
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_scores
                WHERE org_scores.org_id = scored_items.org_id
                AND scored_items.score BETWEEN scored_items.min_score AND scored_items.max_score
                AND org_scores.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(output.contains("BETWEEN"), "Expected BETWEEN in subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn outer_table_ref_unary_not_in_subquery() {
    let sql = r#"
        CREATE TABLE flagged_docs (
            id INT PRIMARY KEY,
            is_hidden INT NOT NULL DEFAULT 0,
            org_id INT NOT NULL
        );
        CREATE TABLE org_access4 (
            id INT PRIMARY KEY,
            org_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE flagged_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY fd_select ON flagged_docs
            FOR SELECT TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access4
                WHERE org_access4.org_id = flagged_docs.org_id
                AND NOT flagged_docs.is_hidden
                AND org_access4.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
        CREATE POLICY fd_insert ON flagged_docs
            FOR INSERT TO authenticated
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY fd_update ON flagged_docs
            FOR UPDATE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access4
                WHERE org_access4.org_id = flagged_docs.org_id
                AND NOT flagged_docs.is_hidden
                AND org_access4.user_id = CAST(current_setting('app.user_id') AS INT)
            ))
            WITH CHECK (org_id = CAST(current_setting('app.user_id') AS INT));
        CREATE POLICY fd_delete ON flagged_docs
            FOR DELETE TO authenticated
            USING (EXISTS (
                SELECT 1 FROM org_access4
                WHERE org_access4.org_id = flagged_docs.org_id
                AND NOT flagged_docs.is_hidden
                AND org_access4.user_id = CAST(current_setting('app.user_id') AS INT)
            ));
    "#;
    let output = translate(sql);
    assert!(output.contains("NOT"), "Expected NOT in subquery: {output}");
    execute_rls_ddl(sql);
}

#[test]
fn missing_current_user_mapping_error() {
    let sql = r#"
        CREATE TABLE user_docs (
            id INT PRIMARY KEY,
            owner TEXT NOT NULL
        );
        ALTER TABLE user_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY ud_select ON user_docs
            FOR SELECT TO authenticated
            USING (owner = current_user);
    "#;
    // Options without current_user mapping
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string());

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);
    assert!(result.is_err(), "Expected error for missing current_user mapping");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("session variable mapping"),
        "Expected session variable mapping error: {err}"
    );
}

#[test]
fn missing_schema_qualified_current_setting_mapping_error() {
    let sql = r#"
        CREATE TABLE scoped_docs (
            id INT PRIMARY KEY,
            owner_id INT NOT NULL
        );
        ALTER TABLE scoped_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY sd_select ON scoped_docs
            FOR SELECT TO authenticated
            USING (owner_id = CAST(pg_catalog.current_setting('app.user_id') AS INT));
    "#;
    // Options intentionally omit current_setting mapping.
    let options = Pg2SqliteOptions::default()
        .with_session_user_role("authenticated".to_string())
        .with_rls_audit_table_name("rls_audit".to_string());

    let result = Pg2Sqlite::default().sql(sql).unwrap().translate(&options);
    assert!(result.is_err(), "Expected error for missing session variable mapping");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("session variable mapping") || err.contains("current_setting"),
        "Expected missing mapping error mentioning current_setting: {err}"
    );
}

#[test]
fn in_subquery_with_outer_table_ref_in_using() {
    let sql = r#"
        CREATE TABLE projects (
            id INT PRIMARY KEY,
            team_id INT NOT NULL
        );
        CREATE TABLE team_members (
            id INT PRIMARY KEY,
            team_id INT NOT NULL,
            user_id INT NOT NULL
        );
        ALTER TABLE projects ENABLE ROW LEVEL SECURITY;
        CREATE POLICY proj_select ON projects
            FOR SELECT TO authenticated
            USING (team_id IN (SELECT team_id FROM team_members WHERE user_id = CAST(current_setting('app.user_id') AS INT)));
        CREATE POLICY proj_insert ON projects
            FOR INSERT TO authenticated
            WITH CHECK (team_id IN (SELECT team_id FROM team_members WHERE user_id = CAST(current_setting('app.user_id') AS INT)));
        CREATE POLICY proj_update ON projects
            FOR UPDATE TO authenticated
            USING (team_id IN (SELECT team_id FROM team_members WHERE user_id = CAST(current_setting('app.user_id') AS INT)))
            WITH CHECK (team_id IN (SELECT team_id FROM team_members WHERE user_id = CAST(current_setting('app.user_id') AS INT)));
        CREATE POLICY proj_delete ON projects
            FOR DELETE TO authenticated
            USING (team_id IN (SELECT team_id FROM team_members WHERE user_id = CAST(current_setting('app.user_id') AS INT)));
    "#;
    let output = translate(sql);
    assert!(output.contains("IN (SELECT"), "Expected IN subquery: {output}");
    execute_rls_ddl(sql);
}
