//! Implementation of the [`Translator`](crate::traits::Translator) trait for
//! the `CreateTable` type.

use alloc::collections::BTreeSet;
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use sqlparser::ast::{
    ColumnOption, ColumnOptionDef, CreateTable, CreateTableOptions, HiveDistributionStyle,
    OnCommit, TableConstraint, WithData,
};

use crate::{
    impls::{
        object_name::normalize_schema_qualified_object_name_for_sqlite,
        translator_impls::column::translate_column_def,
    },
    warnings::TranslationWarning,
};

crate::traits::translator::impl_contextual_translator!(CreateTable => CreateTable);
impl crate::traits::translator::TranslatorWithContext for CreateTable {
    #[allow(clippy::too_many_lines)]
    fn translate_with_warnings(
        &self,
        schema: &Self::Schema,
        options: &crate::options::TranslationContext<'_>,
        emit: &mut dyn FnMut(crate::warnings::TranslationWarning),
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // LIKE t is the most dangerous unsupported clause: SQLite would parse it as
        // a column named LIKE of type t and silently create a table with the wrong
        // schema. Reject before emitting anything.
        if self.like.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {table_name} (LIKE ...) cannot be translated to SQLite. \
                 SQLite would silently accept LIKE as a column name and create a table \
                 with a completely wrong schema. Spell out the columns explicitly instead."
            )));
        }

        // INHERITS has no SQLite equivalent.
        if self.inherits.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {table_name} ... INHERITS (...) cannot be translated to SQLite. \
                 SQLite has no table inheritance. Spell out the inherited columns explicitly."
            )));
        }

        // PARTITION OF has no SQLite equivalent.
        if self.partition_of.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {table_name} PARTITION OF ... cannot be translated to SQLite. \
                 SQLite has no partitioned tables."
            )));
        }

        // PARTITION BY declares a partitioned parent, which SQLite cannot
        // express any more than the PARTITION OF children it would carry.
        if self.partition_by.is_some() {
            let table_name = self.name.to_string();
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {table_name} ... PARTITION BY cannot be translated to SQLite. \
                 SQLite has no partitioned tables."
            )));
        }

        reject_foreign_create_table_modifiers(self)?;

        // GLOBAL and LOCAL are SQL-standard noise words PostgreSQL accepts and
        // ignores before TEMPORARY, measured on 16, so clearing them is
        // provably neutral. A bare GLOBAL without TEMPORARY is refused above,
        // since PostgreSQL rejects that spelling.
        if let Some(global) = self.global {
            emit(TranslationWarning::LossyDrop {
                construct: if global { "GLOBAL" } else { "LOCAL" }.to_string(),
                reason: "PostgreSQL accepts and ignores the word before TEMPORARY, and SQLite \
             has no spelling for it, so it was dropped and the table stays \
             temporary."
                    .to_string(),
            });
        }

        // ON COMMIT PRESERVE ROWS is PostgreSQL's own default for a temporary
        // table, and a SQLite temporary table keeps its rows across
        // transactions anyway, so clearing it changes nothing. The other two
        // dispositions empty or drop the table at commit, which SQLite cannot
        // express, and are refused above.
        if matches!(self.on_commit, Some(OnCommit::PreserveRows)) {
            emit(TranslationWarning::LossyDrop {
                construct: "ON COMMIT PRESERVE ROWS".to_string(),
                reason: "SQLite has no ON COMMIT clause. PRESERVE ROWS is what a SQLite \
             temporary table does anyway, so the clause was dropped."
                    .to_string(),
            });
        }

        // PostgreSQL storage parameters, WITH (fillfactor = 70) and friends,
        // tune the storage engine and never change results, so they are
        // dropped with a warning rather than carried into SQLite, which
        // rejects the clause outright. The other option spellings are foreign
        // and refused above.
        if let CreateTableOptions::With(_) = &self.table_options {
            emit(TranslationWarning::LossyDrop {
                construct: self.table_options.to_string(),
                reason: "PostgreSQL storage parameters tune the storage engine and do not \
             change results, and SQLite has no equivalent clause, so they were \
             dropped."
                    .to_string(),
            });
        }

        // WITH DATA on CREATE TABLE AS restates what both databases do by
        // default, so dropping the words changes nothing. WITH NO DATA would
        // change the table's contents and is refused above.
        if matches!(self.with_data, Some(WithData { data: true, statistics: None })) {
            emit(TranslationWarning::LossyDrop {
                construct: "WITH DATA".to_string(),
                reason: "CREATE TABLE AS populates the new table in both databases, so the \
             clause restates the default and was dropped."
                    .to_string(),
            });
        }

        // UNLOGGED is a durability hint with no SQLite equivalent. Drop it and warn.
        if self.unlogged {
            emit(TranslationWarning::LossyDrop {
                construct: "UNLOGGED".to_string(),
                reason: "SQLite has no UNLOGGED durability setting so the modifier was dropped \
             and the table is created as a regular table."
                    .to_string(),
            });
        }

        // STRICT mode is only valid for regular CREATE TABLE, not CREATE TABLE AS
        // SELECT.
        let is_ctas = self.query.is_some();

        let query = match &self.query {
            Some(q) => Some(Box::new(q.translate_with_warnings(schema, options, emit)?)),
            None => None,
        };

        // The primary key as the table constraints declare it, which a column
        // on its own cannot see and which decides whether it is SQLite's rowid
        // alias. A column that spells `PRIMARY KEY` inline is recognised by
        // `translate_column_def` itself.
        let primary_key_columns: Vec<String> = self
            .constraints
            .iter()
            .filter_map(|constraint| {
                match constraint {
                    TableConstraint::PrimaryKey(primary_key) => Some(&primary_key.columns),
                    _ => None,
                }
            })
            .flatten()
            .map(|column| column.column.to_string())
            .collect();

        // Every field is named so a field added upstream fails to compile here
        // instead of leaking through a spread, the defect this rebuild fixes.
        let mut created_table = Self {
            // The table definition itself, translated.
            name: normalize_schema_qualified_object_name_for_sqlite(schema, &self.name)?,
            columns: self
                .columns
                .iter()
                .map(|c| {
                    translate_column_def(c, &self.name, &primary_key_columns, schema, options, emit)
                })
                .collect::<Result<Vec<_>, _>>()?,
            constraints: self
                .constraints
                .iter()
                .map(|c| c.translate_with_warnings(schema, options, emit))
                .collect::<Result<Vec<Vec<TableConstraint>>, _>>()?
                .into_iter()
                .flatten()
                .collect(),
            query,
            // SQLite STRICT mode enforces type checking (not valid on CTAS).
            strict: !is_ctas,
            // Legal in both databases and carried through unchanged.
            temporary: self.temporary,
            if_not_exists: self.if_not_exists,
            without_rowid: self.without_rowid,
            // Cleared with a warning above when present.
            unlogged: false,
            global: None,
            on_commit: None,
            table_options: CreateTableOptions::None,
            with_data: None,
            // Refused by the checks above when present, so their defaults.
            or_replace: false,
            external: false,
            dynamic: false,
            transient: false,
            volatile: false,
            iceberg: false,
            snapshot: false,
            hive_distribution: HiveDistributionStyle::NONE,
            hive_formats: None,
            file_format: None,
            location: None,
            like: None,
            clone: None,
            version: None,
            comment: None,
            on_cluster: None,
            primary_key: None,
            order_by: None,
            partition_by: None,
            cluster_by: None,
            clustered_by: None,
            inherits: None,
            partition_of: None,
            for_values: None,
            copy_grants: false,
            enable_schema_evolution: None,
            change_tracking: None,
            data_retention_time_in_days: None,
            max_data_extension_time_in_days: None,
            default_ddl_collation: None,
            with_aggregation_policy: None,
            with_row_access_policy: None,
            with_storage_lifecycle_policy: None,
            with_tags: None,
            external_volume: None,
            with_connection: None,
            base_location: None,
            catalog: None,
            catalog_sync: None,
            storage_serialization_policy: None,
            target_lag: None,
            warehouse: None,
            refresh_mode: None,
            initialize: None,
            require_user: false,
            diststyle: None,
            distkey: None,
            sortkey: None,
            backup: None,
            multiset: None,
            fallback: None,
        };

        let mut pk_column_names = BTreeSet::new();

        for constraint in &created_table.constraints {
            if let TableConstraint::PrimaryKey(pk_constraint) = constraint {
                for col in &pk_constraint.columns {
                    if let sqlparser::ast::Expr::Identifier(ident) = &col.column.expr {
                        pk_column_names.insert(ident.value.clone());
                    }
                }
            }
        }

        for col in &created_table.columns {
            for option in &col.options {
                if let ColumnOption::PrimaryKey(_) = &option.option {
                    pk_column_names.insert(col.name.value.clone());
                }
            }
        }

        for col in &mut created_table.columns {
            if pk_column_names.contains(&col.name.value)
                && !col.options.iter().any(|o| matches!(o.option, ColumnOption::NotNull))
            {
                col.options.push(ColumnOptionDef { name: None, option: ColumnOption::NotNull });
            }
        }

        Ok(created_table)
    }
}

/// Refuses the `CREATE TABLE` modifiers that cannot reach SQLite, so
/// [`Translator::translate`](crate::traits::Translator::translate) can rebuild
/// the statement naming every field.
///
/// Two kinds land here. The foreign spellings are refused because PostgreSQL
/// rejects them, so a file carrying one is not the input this crate accepts.
/// Most cannot parse under the PostgreSQL dialect today, and all are named
/// anyway so a parser change cannot reopen the hole quietly. The rest are
/// valid PostgreSQL whose loss would change results: ON COMMIT DELETE ROWS
/// and ON COMMIT DROP alter what a transaction leaves behind, and WITH NO
/// DATA creates the table empty, none of which SQLite can express.
///
/// GLOBAL and LOCAL before TEMPORARY, ON COMMIT PRESERVE ROWS, WITH storage
/// parameters, WITH DATA, and UNLOGGED never reach a refusal: each restates a
/// default or tunes storage without changing results, and the caller clears
/// it with a warning instead.
#[allow(clippy::too_many_lines)]
fn reject_foreign_create_table_modifiers(
    create_table: &CreateTable,
) -> Result<(), crate::errors::Error> {
    let foreign = if create_table.global.is_some() && !create_table.temporary {
        // PostgreSQL only accepts GLOBAL and LOCAL immediately before
        // TEMPORARY, measured on 16.
        Some(if create_table.global == Some(true) {
            "GLOBAL without TEMPORARY"
        } else {
            "LOCAL without TEMPORARY"
        })
    } else if create_table.or_replace {
        // PostgreSQL replaces views and functions, never tables.
        Some("OR REPLACE, which is Snowflake")
    } else if create_table.external {
        Some("EXTERNAL, which is Hive")
    } else if create_table.dynamic {
        Some("DYNAMIC, which is Snowflake")
    } else if create_table.transient {
        Some("TRANSIENT, which is Snowflake")
    } else if create_table.volatile {
        Some("VOLATILE, which is Teradata")
    } else if create_table.iceberg {
        Some("ICEBERG, which is Snowflake")
    } else if create_table.snapshot {
        Some("SNAPSHOT, which is BigQuery")
    } else if matches!(create_table.hive_distribution, HiveDistributionStyle::PARTITIONED { .. }) {
        Some("PARTITIONED BY, which is Hive")
    } else if matches!(create_table.hive_distribution, HiveDistributionStyle::SKEWED { .. }) {
        Some("SKEWED BY, which is Hive")
    } else if create_table.hive_formats.is_some() {
        Some("ROW FORMAT, STORED AS, or LOCATION, which is Hive")
    } else if matches!(create_table.table_options, CreateTableOptions::Options(_)) {
        Some("OPTIONS(...), which is BigQuery")
    } else if matches!(create_table.table_options, CreateTableOptions::TableProperties(_)) {
        Some("TBLPROPERTIES, which is Hive")
    } else if matches!(create_table.table_options, CreateTableOptions::Plain(_)) {
        Some("a plain options list such as ENGINE, which is MySQL")
    } else if create_table.file_format.is_some() {
        Some("a STORED AS file format, which is Hive")
    } else if create_table.location.is_some() {
        Some("LOCATION, which is Hive")
    } else if create_table.clone.is_some() {
        Some("CLONE, which is Snowflake")
    } else if create_table.version.is_some() {
        Some("a table VERSION clause")
    } else if create_table.comment.is_some() {
        Some("an inline COMMENT, which is Hive")
    } else if create_table.on_cluster.is_some() {
        Some("ON CLUSTER, which is ClickHouse")
    } else if create_table.primary_key.is_some() {
        Some("a trailing PRIMARY KEY clause, which is ClickHouse")
    } else if create_table.order_by.is_some() {
        Some("a trailing ORDER BY clause, which is ClickHouse")
    } else if create_table.cluster_by.is_some() {
        Some("CLUSTER BY, which is BigQuery or Snowflake")
    } else if create_table.clustered_by.is_some() {
        Some("CLUSTERED BY, which is Hive")
    } else if create_table.copy_grants {
        Some("COPY GRANTS, which is Snowflake")
    } else if create_table.enable_schema_evolution.is_some() {
        Some("ENABLE_SCHEMA_EVOLUTION, which is Snowflake")
    } else if create_table.change_tracking.is_some() {
        Some("CHANGE_TRACKING, which is Snowflake")
    } else if create_table.data_retention_time_in_days.is_some() {
        Some("DATA_RETENTION_TIME_IN_DAYS, which is Snowflake")
    } else if create_table.max_data_extension_time_in_days.is_some() {
        Some("MAX_DATA_EXTENSION_TIME_IN_DAYS, which is Snowflake")
    } else if create_table.default_ddl_collation.is_some() {
        Some("DEFAULT_DDL_COLLATION, which is Snowflake")
    } else if create_table.with_aggregation_policy.is_some() {
        Some("WITH AGGREGATION POLICY, which is Snowflake")
    } else if create_table.with_row_access_policy.is_some() {
        Some("WITH ROW ACCESS POLICY, which is Snowflake")
    } else if create_table.with_storage_lifecycle_policy.is_some() {
        Some("WITH STORAGE LIFECYCLE POLICY, which is Snowflake")
    } else if create_table.with_tags.is_some() {
        Some("WITH TAG, which is Snowflake")
    } else if create_table.external_volume.is_some() {
        Some("EXTERNAL_VOLUME, which is Snowflake")
    } else if create_table.with_connection.is_some() {
        Some("WITH CONNECTION, which is BigQuery")
    } else if create_table.base_location.is_some() {
        Some("BASE_LOCATION, which is Snowflake")
    } else if create_table.catalog.is_some() {
        Some("CATALOG, which is Snowflake")
    } else if create_table.catalog_sync.is_some() {
        Some("CATALOG_SYNC, which is Snowflake")
    } else if create_table.storage_serialization_policy.is_some() {
        Some("STORAGE_SERIALIZATION_POLICY, which is Snowflake")
    } else if create_table.target_lag.is_some() {
        Some("TARGET_LAG, which is Snowflake")
    } else if create_table.warehouse.is_some() {
        Some("WAREHOUSE, which is Snowflake")
    } else if create_table.refresh_mode.is_some() {
        Some("REFRESH_MODE, which is Snowflake")
    } else if create_table.initialize.is_some() {
        Some("INITIALIZE, which is Snowflake")
    } else if create_table.require_user {
        Some("REQUIRE USER, which is Snowflake")
    } else if create_table.diststyle.is_some() {
        Some("DISTSTYLE, which is Redshift")
    } else if create_table.distkey.is_some() {
        Some("DISTKEY, which is Redshift")
    } else if create_table.sortkey.is_some() {
        Some("SORTKEY, which is Redshift")
    } else if create_table.backup.is_some() {
        Some("BACKUP, which is Redshift")
    } else if create_table.multiset == Some(true) {
        Some("MULTISET, which is Teradata")
    } else if create_table.multiset == Some(false) {
        Some("SET, which is Teradata")
    } else if create_table.fallback.is_some() {
        Some("FALLBACK, which is Teradata")
    } else if matches!(create_table.with_data, Some(WithData { statistics: Some(_), .. })) {
        Some("AND STATISTICS, which is Teradata")
    } else {
        None
    };

    if let Some(clause) = foreign {
        return Err(crate::errors::Error::forward_refusal(format!(
            "CREATE TABLE {} carries {clause}. PostgreSQL rejects that spelling, so a file \
             containing it is not the input this crate translates. Remove the clause.",
            create_table.name
        )));
    }

    match create_table.on_commit {
        Some(OnCommit::DeleteRows) => {
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {} ... ON COMMIT DELETE ROWS empties the table at every commit, \
                 and a SQLite temporary table always keeps its rows, so dropping the clause \
                 would change what a transaction leaves behind. Remove it and empty the table \
                 explicitly where needed.",
                create_table.name
            )));
        }
        Some(OnCommit::Drop) => {
            return Err(crate::errors::Error::forward_refusal(format!(
                "CREATE TABLE {} ... ON COMMIT DROP removes the table at commit, which SQLite \
                 cannot express, so dropping the clause would leave a table behind that \
                 PostgreSQL would have removed. Remove it and drop the table explicitly where \
                 needed.",
                create_table.name
            )));
        }
        Some(OnCommit::PreserveRows) | None => {}
    }

    if matches!(create_table.with_data, Some(WithData { data: false, .. })) {
        return Err(crate::errors::Error::forward_refusal(format!(
            "CREATE TABLE {} AS ... WITH NO DATA creates the table empty, and SQLite's CREATE \
             TABLE AS always populates it, so dropping the clause would change the table's \
             contents. Spell out the columns in a plain CREATE TABLE instead.",
            create_table.name
        )));
    }

    Ok(())
}
