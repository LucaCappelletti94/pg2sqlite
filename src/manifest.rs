//! Logical to physical table map for a translation.
//!
//! RLS translation renames a table's storage behind a view and read-only
//! translation denies writes, so the logical (PostgreSQL) name no longer
//! always names a plain table. The RLS suffix is configurable, so the map
//! cannot be guessed by convention. Get it from
//! [`Pg2Sqlite::translation_manifest`](crate::pg2sqlite::Pg2Sqlite::translation_manifest).

#[cfg(not(feature = "std"))]
use alloc::string::String;

/// How the translation wrapped one table.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapperKind {
    /// Translated one to one. The logical name is a real table.
    Plain,
    /// RLS translation. The physical table carries the configured suffix, a
    /// view holds the logical name, and INSTEAD OF triggers enforce policies.
    RlsView,
    /// Read-only translation. The name is unchanged and BEFORE triggers deny
    /// writes.
    ReadOnly,
}

/// One table's translation outcome.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableManifestEntry {
    /// The table name in the source (PostgreSQL) schema.
    pub logical: String,
    /// The SQLite table that physically stores the rows.
    pub physical: String,
    /// The wrapper generated around the physical table.
    pub wrapper: WrapperKind,
    /// How each column is represented, for the columns whose representation a
    /// consumer cannot infer from the emitted type alone.
    pub columns: Vec<ColumnManifestEntry>,
}

/// How one column is physically represented.
///
/// A `NUMERIC(p,s)` column is emitted as an INTEGER holding minor units, so
/// reading it back gives 1999 where PostgreSQL gave 19.99. Dividing by `10^s`
/// in the projection would put the value back into a float and undo the point
/// of the representation, so the scale is published here and the consumer
/// applies it deliberately.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnManifestEntry {
    /// The column name, as declared in the source schema.
    pub name: String,
    /// The power of ten the stored integer is scaled by, for a `NUMERIC` or
    /// `DECIMAL` column, and `None` for every other type.
    pub minor_unit_scale: Option<u32>,
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sqlparser::ast::Statement;

    use super::WrapperKind;
    use crate::{
        pg2sqlite::Pg2Sqlite,
        prelude::{Pg2SqliteOptions, TranslationOptions},
    };

    fn manifest(sql: &str, options: &Pg2SqliteOptions) -> Vec<super::TableManifestEntry> {
        Pg2Sqlite::default()
            .sql(sql)
            .expect("input should parse")
            .translation_manifest(options)
            .expect("manifest should build")
    }

    /// Backing-table name of the sole `CREATE TABLE` carrying the RLS suffix in
    /// the generator's real output. Used as the drift-guard oracle.
    fn generated_rls_backing_name(sql: &str, options: &Pg2SqliteOptions) -> String {
        let statements = Pg2Sqlite::default()
            .sql(sql)
            .expect("input should parse")
            .translate(options)
            .expect("translation should succeed");
        let suffix = options.get_rls_table_suffix();
        let names: Vec<String> = statements
            .iter()
            .filter_map(|statement| {
                match statement {
                    Statement::CreateTable(create) => Some(create.name.to_string()),
                    _ => None,
                }
            })
            .filter(|name| name.contains(suffix))
            .collect();
        assert_eq!(names.len(), 1, "expected exactly one RLS backing table, got {names:?}");
        names.into_iter().next().unwrap()
    }

    const PLAIN_AND_RLS: &str = r#"
        CREATE TABLE plain_docs (id INTEGER PRIMARY KEY, body TEXT);
        CREATE TABLE secure_docs (id INTEGER PRIMARY KEY, owner_id INTEGER);
        ALTER TABLE secure_docs ENABLE ROW LEVEL SECURITY;
        CREATE POLICY p ON secure_docs USING (owner_id = 1);
    "#;

    #[test]
    fn plain_and_rls_tables_yield_expected_entries_with_drift_guarded_physical_name() {
        let options = Pg2SqliteOptions::default().with_rls_audit_table_name("rls_audit");
        let manifest = manifest(PLAIN_AND_RLS, &options);

        assert_eq!(manifest.len(), 2);

        assert_eq!(manifest[0].logical, "plain_docs");
        assert_eq!(manifest[0].physical, "plain_docs");
        assert_eq!(manifest[0].wrapper, WrapperKind::Plain);

        assert_eq!(manifest[1].logical, "secure_docs");
        assert_eq!(manifest[1].physical, "secure_docs_rls");
        assert_eq!(manifest[1].wrapper, WrapperKind::RlsView);

        let backing = generated_rls_backing_name(PLAIN_AND_RLS, &options);
        assert!(
            backing.contains(&manifest[1].physical),
            "manifest physical {} drifted from generated backing table {backing}",
            manifest[1].physical,
        );
    }

    #[test]
    fn rls_table_suffix_option_is_reflected_in_manifest() {
        let options = Pg2SqliteOptions::default()
            .with_rls_table_suffix("_x")
            .with_rls_audit_table_name("rls_audit");
        let manifest = manifest(PLAIN_AND_RLS, &options);

        let rls_entry = manifest.iter().find(|e| e.logical == "secure_docs").unwrap();
        assert_eq!(rls_entry.physical, "secure_docs_x");
        assert_eq!(rls_entry.wrapper, WrapperKind::RlsView);

        let backing = generated_rls_backing_name(PLAIN_AND_RLS, &options);
        assert!(backing.contains("secure_docs_x"), "generated backing table was {backing}");
    }

    #[test]
    fn empty_schema_yields_empty_manifest() {
        assert!(manifest("CREATE ROLE nobody;", &Pg2SqliteOptions::default()).is_empty());
    }

    #[test]
    fn readonly_non_rls_table_is_classified_read_only_with_equal_names() {
        let sql = r#"
            CREATE ROLE app_user;
            CREATE TABLE reference_data (id INTEGER PRIMARY KEY, label TEXT);
            GRANT SELECT ON reference_data TO app_user;
            CREATE TABLE editable (id INTEGER PRIMARY KEY);
            GRANT ALL ON editable TO app_user;
        "#;
        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let manifest = manifest(sql, &options);

        let readonly = manifest.iter().find(|e| e.logical == "reference_data").unwrap();
        assert_eq!(readonly.physical, "reference_data");
        assert_eq!(readonly.wrapper, WrapperKind::ReadOnly);

        let writable = manifest.iter().find(|e| e.logical == "editable").unwrap();
        assert_eq!(writable.physical, "editable");
        assert_eq!(writable.wrapper, WrapperKind::Plain);
    }

    #[test]
    fn non_selectable_table_is_omitted_from_manifest() {
        let sql = r#"
            CREATE ROLE app_user;
            CREATE TABLE hidden (id INTEGER PRIMARY KEY);
            CREATE TABLE visible (id INTEGER PRIMARY KEY);
            GRANT SELECT ON visible TO app_user;
        "#;
        let options = Pg2SqliteOptions::default().with_session_user_role("app_user");
        let manifest = manifest(sql, &options);

        assert_eq!(
            manifest.iter().map(|e| e.logical.as_str()).collect::<Vec<_>>(),
            vec!["visible"]
        );
    }
}
