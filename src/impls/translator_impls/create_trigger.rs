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

use sql_traits::{
    errors::LookupError,
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike, TriggerLike},
};
use sqlparser::{
    ast::{
        Assignment, AssignmentTarget, BinaryOperator, ConditionalStatements, CreateTrigger,
        DropTrigger, Expr, Ident, ObjectName, ObjectNamePart, Statement, TableFactor,
        TableWithJoins, TriggerEvent, TriggerExecBodyType, TriggerObject, TriggerObjectKind,
        TriggerPeriod, Update, Value, ValueWithSpan, helpers::attached_token::AttachedToken,
    },
    keywords::Keyword,
    tokenizer::{Span, Token, TokenWithSpan, Word},
};

use crate::{
    impls::{
        object_name::{
            append_suffix, normalize_schema_qualified_object_name_for_sqlite,
            table_has_implicit_public_rls, table_with_implicit_public_lookup,
            validate_schema_qualified_object_name_for_sqlite,
        },
        query_builder::single_expr_query,
        shared_helpers::ColumnRewrites,
    },
    options::Pg2SqliteOptions,
    traits::{schema::Schema, translation_options::TranslationOptions, translator::Translator},
};

/// Builds the `UPDATE` that stands in for a plpgsql body assigning to
/// `NEW.<column>`, which SQLite has no way to express directly.
///
/// The assigned value is PostgreSQL and goes through `Expr::translate` like any
/// other expression. Skipping that step emitted `AT TIME ZONE`, `::` casts, and
/// `greatest` verbatim, the first two rejected when the trigger is created and
/// the third only on the first write, since SQLite resolves function names
/// lazily.
fn generate_maintenance_trigger_body(
    trigger: &CreateTrigger,
    target_table_name: &ObjectName,
    row_context: &str,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<sqlparser::ast::BeginEndStatements, crate::errors::Error> {
    // The rewrites come from the trigger's own table, whose name is still the
    // PostgreSQL one at this point. `target_table_name` may already be the
    // redirected RLS backing table, which the schema does not hold.
    let rewrites = ColumnRewrites::for_named_table(schema, &trigger.table_name, options);
    let assignments = trigger
        .maintenance_assignments(schema)?
        .map(|(col, expr)| {
            let value = expr.translate(schema, options)?;
            Ok(Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    Ident::new(col.column_name()),
                )])),
                value: rewrites.finish_value(col.column_name(), value, options)?,
            })
        })
        .collect::<Result<Vec<_>, crate::errors::Error>>()?;

    let update_stmt = Statement::Update(Update {
        update_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
            value: "UPDATE".into(),
            quote_style: None,
            keyword: Keyword::UPDATE,
        }))),
        table: TableWithJoins {
            relation: TableFactor::Table {
                name: target_table_name.clone(),
                alias: None,
                args: None,
                with_hints: vec![],
                version: None,
                partitions: vec![],
                json_path: None,
                sample: None,
                index_hints: vec![],
                with_ordinality: false,
            },
            joins: vec![],
        },
        assignments,
        from: None,
        selection: Some(Expr::BinaryOp {
            left: Box::new(Expr::Identifier(Ident::new("rowid"))),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new(row_context),
                Ident::new("rowid"),
            ])),
        }),
        returning: None,
        output: None,
        or: None,
        order_by: Vec::new(),
        limit: None,
        optimizer_hints: Vec::new(),
    });

    Ok(sqlparser::ast::BeginEndStatements {
        begin_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
            value: "BEGIN".into(),
            quote_style: None,
            keyword: Keyword::BEGIN,
        }))),
        statements: vec![update_stmt],
        end_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
            value: "END".into(),
            quote_style: None,
            keyword: Keyword::END,
        }))),
    })
}

/// Replaces an empty translated body with `SELECT NULL`, reporting the trigger
/// whose body vanished.
///
/// The report is unconditional because nothing here can tell an intentional
/// no-op, a plpgsql function whose only statement is `RETURN NEW`, from a body
/// the plpgsql translator emptied by dropping statements it could not render.
fn substitute_no_op_body_when_empty(
    mut body: sqlparser::ast::BeginEndStatements,
) -> sqlparser::ast::BeginEndStatements {
    if !body.statements.is_empty() {
        return body;
    }

    crate::warnings::emit(crate::warnings::TranslationWarning::LossyDrop {
        construct: "empty trigger body".to_string(),
        reason: "the translated trigger body has no statements left, so the trigger does nothing. \
                 SQLite rejects an empty BEGIN END, so it carries SELECT NULL instead."
            .to_string(),
    });

    // SQLite rejects an empty BEGIN END, so the body carries `SELECT NULL`.
    body.statements = vec![Statement::Query(Box::new(single_expr_query(
        Expr::Value(ValueWithSpan { value: Value::Null, span: Span::empty() }),
        vec![],
        None,
    )))];
    body
}

fn generate_standard_trigger_body(
    exec_body: &sqlparser::ast::TriggerExecBody,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::BeginEndStatements>, crate::errors::Error> {
    let function_name = exec_body.func_desc.name.clone();
    if let Some((mut body, context)) =
        schema.function_body_with_context(&function_name.to_string())?
    {
        body.statements = super::plpgsql::PlPgSqlTranslator::translate_with_context(
            &body, context, schema, options,
        )?;
        Ok(Some(body))
    } else {
        Ok(None)
    }
}

fn collect_non_maintenance_update_columns(
    trigger: &CreateTrigger,
    schema: &ParserDB,
    maintenance_columns: &BTreeSet<String>,
) -> Result<Vec<Ident>, LookupError> {
    let Ok(Some(table)) = table_with_implicit_public_lookup(schema, &trigger.table_name) else {
        return Ok(vec![]);
    };

    Ok(table
        .columns(schema)?
        .filter_map(|column| {
            let name = column.column_name();
            (!maintenance_columns.contains(&name.to_lowercase())).then(|| Ident::new(name))
        })
        .collect())
}

fn rewrite_maintenance_update_events(
    trigger: &CreateTrigger,
    events: Vec<TriggerEvent>,
    schema: &ParserDB,
) -> Result<Vec<TriggerEvent>, LookupError> {
    let maintenance_columns = trigger
        .maintenance_assignments(schema)?
        .map(|(column, _)| column.column_name().to_lowercase())
        .collect::<BTreeSet<_>>();

    if maintenance_columns.is_empty() {
        return Ok(events);
    }

    let non_maintenance_columns =
        collect_non_maintenance_update_columns(trigger, schema, &maintenance_columns)?;

    Ok(events
        .into_iter()
        .map(|event| {
            match event {
                TriggerEvent::Update(columns) if columns.is_empty() => {
                    if non_maintenance_columns.is_empty() {
                        TriggerEvent::Update(columns)
                    } else {
                        TriggerEvent::Update(non_maintenance_columns.clone())
                    }
                }
                TriggerEvent::Update(columns) => {
                    let filtered_columns = columns
                        .iter()
                        .filter(|column| {
                            !maintenance_columns.contains(&column.value.to_lowercase())
                        })
                        .cloned()
                        .collect::<Vec<_>>();

                    if filtered_columns.is_empty() {
                        TriggerEvent::Update(columns)
                    } else {
                        TriggerEvent::Update(filtered_columns)
                    }
                }
                other => other,
            }
        })
        .collect())
}

fn maintenance_trigger_has_insert_event(events: &[TriggerEvent]) -> bool {
    events.iter().any(|event| matches!(event, TriggerEvent::Insert))
}

fn split_before_insert_maintenance_trigger(
    create_trigger: &CreateTrigger,
    schema: &ParserDB,
) -> Option<(CreateTrigger, CreateTrigger)> {
    let Ok(true) = create_trigger.is_maintenance_trigger(schema) else {
        return None;
    };

    if !matches!(create_trigger.period, Some(TriggerPeriod::Before)) {
        return None;
    }

    let has_insert_event =
        create_trigger.events.iter().any(|event| matches!(event, TriggerEvent::Insert));
    if !has_insert_event {
        return None;
    }

    let non_insert_events = create_trigger
        .events
        .iter()
        .filter(|event| !matches!(event, TriggerEvent::Insert))
        .cloned()
        .collect::<Vec<_>>();
    if non_insert_events.is_empty() {
        return None;
    }

    let mut insert_trigger = create_trigger.clone();
    insert_trigger.events = vec![TriggerEvent::Insert];
    insert_trigger.name = append_suffix(&create_trigger.name, "_pg2sqlite_insert");

    let mut non_insert_trigger = create_trigger.clone();
    non_insert_trigger.events = non_insert_events;

    Some((insert_trigger, non_insert_trigger))
}

impl Translator for CreateTrigger {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Vec<(Option<DropTrigger>, Self)>;

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        // Checked before the FOR EACH clause because PostgreSQL allows
        // TRUNCATE triggers only FOR EACH STATEMENT, so that check would catch
        // every valid one first and advise a rewrite PostgreSQL rejects.
        if self.events.iter().any(|event| matches!(event, TriggerEvent::Truncate)) {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                "a TRUNCATE trigger has no SQLite equivalent, since SQLite has no TRUNCATE. \
                 PostgreSQL TRUNCATE is translated to DELETE FROM, which fires DELETE triggers, \
                 so add the work to a DELETE trigger if it should run on that."
                    .to_string(),
            ));
        }

        // SQLite has only row triggers. The omitted clause is the same case:
        // PostgreSQL defaults to STATEMENT and SQLite to ROW, so passing it
        // through reverses how often the body runs. Checked before the
        // maintenance-trigger split, which recurses through here.
        match self.trigger_object {
            Some(
                TriggerObjectKind::For(TriggerObject::Row)
                | TriggerObjectKind::ForEach(TriggerObject::Row),
            ) => {}
            Some(
                TriggerObjectKind::For(TriggerObject::Statement)
                | TriggerObjectKind::ForEach(TriggerObject::Statement),
            ) => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "a statement trigger has no SQLite equivalent, since SQLite fires a trigger \
                     once per row rather than once per statement. Rewrite the body so it is \
                     correct once per row and declare the trigger FOR EACH ROW."
                        .to_string(),
                ));
            }
            None => {
                return Err(crate::errors::Error::UnsupportedSQLiteFeature(
                    "a trigger with no FOR EACH clause is a statement trigger in PostgreSQL, \
                     which has no SQLite equivalent, since SQLite fires a trigger once per row \
                     rather than once per statement. Write FOR EACH ROW if that is what was \
                     meant, since SQLite would otherwise silently run the body once per row."
                        .to_string(),
                ));
            }
        }

        // A transition table hands the body every row the statement touched,
        // which a SQLite row trigger cannot see. PostgreSQL allows the clause
        // on a FOR EACH ROW trigger too, so R27's rejection does not cover it.
        if let Some(referencing) = self.referencing.first() {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "the transition table `{referencing}` has no SQLite equivalent, since a SQLite \
                 trigger body sees one row at a time through NEW and OLD and never the set of \
                 rows a statement touched. Collect the rows in a table the body appends to, or \
                 do the work in the application."
            )));
        }

        let source_table_name = self.table_name.clone();
        validate_schema_qualified_object_name_for_sqlite(schema, &source_table_name)?;
        let normalized_source_table_name =
            normalize_schema_qualified_object_name_for_sqlite(schema, &source_table_name)?;

        let mut normalized_trigger = self.clone();
        normalized_trigger.table_name = normalized_source_table_name;

        let can_use_trigger_traits = normalized_trigger
            .table_name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .is_some_and(|ident| schema.table(None, ident.value.as_str()).is_some());

        if can_use_trigger_traits
            && let Some((insert_trigger, non_insert_trigger)) =
                split_before_insert_maintenance_trigger(&normalized_trigger, schema)
        {
            let mut translated = Vec::new();
            translated.extend(non_insert_trigger.translate(schema, options)?);
            translated.extend(insert_trigger.translate(schema, options)?);
            return Ok(translated);
        }

        let trigger_for_helpers = normalized_trigger.clone();

        let CreateTrigger {
            or_alter,
            temporary,
            or_replace,
            is_constraint,
            name,
            period,
            events,
            table_name: _table_name,
            referenced_table_name,
            referencing,
            trigger_object,
            period_before_table,
            condition,
            statements_as,
            exec_body,
            statements,
            characteristics,
        } = normalized_trigger;

        if let Some(statements) = statements {
            return Err(crate::errors::Error::UnknownPostgresFeature(format!(
                "Triggers with statements are not supported: `{statements}`"
            )));
        }

        let Some(exec_body) = exec_body else {
            return Err(crate::errors::Error::UnknownPostgresFeature(
                "Triggers without an execution body are not supported".into(),
            ));
        };

        if matches!(exec_body.exec_type, TriggerExecBodyType::Procedure) {
            return Err(crate::errors::Error::UnknownPostgresFeature(format!(
                "Triggers with execution body of type `Procedure` are not supported: `{exec_body}`"
            )));
        }

        let mut period = period;
        let is_maintenance_trigger =
            can_use_trigger_traits && trigger_for_helpers.is_maintenance_trigger(schema)?;
        let events = if is_maintenance_trigger {
            rewrite_maintenance_update_events(&trigger_for_helpers, events, schema)?
        } else {
            events
        };
        let maintenance_insert_event =
            is_maintenance_trigger && maintenance_trigger_has_insert_event(&events);
        if maintenance_insert_event && matches!(period, Some(TriggerPeriod::Before)) {
            // SQLite cannot apply row maintenance updates for INSERT in BEFORE timing
            // because the row does not exist yet; translate to AFTER to
            // preserve final-row semantics.
            period = Some(TriggerPeriod::After);
        }

        // For BEFORE/AFTER triggers on RLS-protected tables, redirect to the underlying
        // _rls table. INSTEAD OF triggers are used on the view, but
        // BEFORE/AFTER triggers must target the actual table (which has been
        // renamed to table_rls).
        let redirected_source_table_name =
            if matches!(period, Some(TriggerPeriod::Before | TriggerPeriod::After)) {
                if table_has_implicit_public_rls(schema, &source_table_name)? {
                    append_suffix(&source_table_name, options.get_rls_table_suffix())
                } else {
                    source_table_name.clone()
                }
            } else {
                source_table_name.clone()
            };
        let redirected_table_name = normalize_schema_qualified_object_name_for_sqlite(
            schema,
            &redirected_source_table_name,
        )?;

        let function_body = if is_maintenance_trigger {
            let row_context = if maintenance_insert_event { "NEW" } else { "OLD" };
            generate_maintenance_trigger_body(
                &trigger_for_helpers,
                &redirected_table_name,
                row_context,
                schema,
                options,
            )?
        } else if let Some(body) = generate_standard_trigger_body(&exec_body, schema, options)? {
            body
        } else {
            return Err(crate::errors::Error::UnsupportedSQLiteFeature(format!(
                "Trigger function '{}' body not found. Make sure the CREATE FUNCTION statement \
                 is included in the same translation batch as the CREATE TRIGGER.",
                exec_body.func_desc.name
            )));
        };

        // SQLite requires at least one statement between BEGIN and END, so an
        // emptied body is `near "END": syntax error` rather than a trigger that
        // does nothing. Substituting `SELECT NULL` keeps the trigger object in
        // place, which skipping the statement would not: a later DROP TRIGGER
        // would find nothing, and a CREATE OR REPLACE could not emit its DROP
        // at all, since this returns pairs in which the CREATE is not optional.
        let function_body = substitute_no_op_body_when_empty(function_body);

        let maybe_drop_trigger = or_replace.then(|| {
            DropTrigger {
                if_exists: true,
                trigger_name: name.clone(),
                table_name: None,
                option: None,
            }
        });

        if or_alter {
            return Err(crate::errors::Error::UnknownPostgresFeature(
                "Triggers with `OR ALTER` are not supported".into(),
            ));
        }

        if is_constraint {
            return Err(crate::errors::Error::UnknownPostgresFeature(
                "Constraint triggers are not supported".into(),
            ));
        }

        if let Some(characteristics) = &characteristics {
            return Err(crate::errors::Error::UnknownPostgresFeature(format!(
                "Triggers with characteristics are not supported: `{characteristics}`"
            )));
        }

        Ok(vec![(
            maybe_drop_trigger,
            CreateTrigger {
                or_alter,
                temporary,
                or_replace: false,
                is_constraint,
                name,
                period,
                events,
                table_name: redirected_table_name,
                referenced_table_name,
                referencing,
                trigger_object,
                period_before_table,
                statements_as,
                condition: condition
                    .as_ref()
                    .map(|cond| cond.translate(schema, options))
                    .transpose()?,
                exec_body: None,
                statements: Some(ConditionalStatements::BeginEnd(function_body)),
                characteristics: None,
            },
        )])
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use sql_traits::structs::ParserDB;
    use sqlparser::{
        ast::{CreateTrigger, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    use crate::prelude::{Pg2SqliteOptions, Translator};

    fn parse_statements(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).expect("sql should parse")
    }

    fn parse_trigger(sql: &str) -> CreateTrigger {
        let stmt = parse_statements(sql).remove(0);
        let Statement::CreateTrigger(trigger) = stmt else {
            panic!("expected create trigger");
        };
        trigger
    }

    fn schema_with_trigger_function_and_rls_table() -> ParserDB {
        let schema_sql = r#"
            CREATE TABLE docs(id INTEGER PRIMARY KEY);
            ALTER TABLE docs ENABLE ROW LEVEL SECURITY;
            CREATE FUNCTION docs_trigger_fn() RETURNS trigger AS $$
            BEGIN
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
        "#;
        ParserDB::from_statements(parse_statements(schema_sql), "test".to_string())
            .expect("schema should build")
    }

    #[test]
    fn instead_of_trigger_on_rls_table_keeps_original_table_name() {
        let schema = schema_with_trigger_function_and_rls_table();
        let options = Pg2SqliteOptions::default();
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_instead INSTEAD OF INSERT ON docs \
             FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()",
        );

        let translated = trigger
            .translate(&schema, &options)
            .expect("trigger translation should succeed")
            .into_iter()
            .next()
            .expect("trigger should be translated");

        let (_drop_stmt, create_trigger) = translated;
        assert_eq!(create_trigger.table_name.to_string(), "docs");
    }

    #[test]
    fn missing_trigger_function_body_always_errors() {
        let schema = ParserDB::from_statements(
            parse_statements("CREATE TABLE docs(id INTEGER PRIMARY KEY);"),
            "test".to_string(),
        )
        .expect("schema should build");
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
             FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()",
        );

        let err =
            trigger.translate(&schema, &Pg2SqliteOptions::default()).expect_err("should fail");
        assert!(err.to_string().contains("Trigger function"), "unexpected error: {err}");
    }

    #[test]
    fn before_insert_or_update_maintenance_trigger_translates_to_two_triggers() {
        let schema = ParserDB::from_statements(
            parse_statements(
                r#"
                CREATE TABLE brands(id INTEGER PRIMARY KEY, name TEXT, edited_at TEXT);
                CREATE FUNCTION set_brands_edited_at() RETURNS trigger AS $$
                BEGIN
                    NEW.edited_at = CURRENT_TIMESTAMP;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                "#,
            ),
            "test".to_string(),
        )
        .expect("schema should build");
        let trigger = parse_trigger(
            "CREATE TRIGGER trigger_upsert_brands_edited_at \
             BEFORE INSERT OR UPDATE ON brands \
             FOR EACH ROW EXECUTE FUNCTION set_brands_edited_at()",
        );

        let translated = trigger
            .translate(&schema, &Pg2SqliteOptions::default())
            .expect("trigger translation should succeed");
        assert_eq!(translated.len(), 2, "expected split translation for mixed maintenance trigger");

        let sql = translated
            .into_iter()
            .map(|(_, trigger)| trigger.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            sql.contains(
                "CREATE TRIGGER trigger_upsert_brands_edited_at BEFORE UPDATE OF id, name ON brands"
            ),
            "missing BEFORE UPDATE branch: {sql}"
        );
        assert!(
            sql.contains("CREATE TRIGGER trigger_upsert_brands_edited_at_pg2sqlite_insert AFTER INSERT ON brands"),
            "missing AFTER INSERT branch: {sql}"
        );
    }
    /// SQLite has only row triggers, and `FOR EACH STATEMENT` is `near
    /// "STATEMENT": syntax error`. A row trigger cannot stand in for one: the
    /// body would run once per affected row instead of once per statement.
    #[test]
    fn statement_triggers_are_rejected() {
        for spelling in ["FOR EACH STATEMENT", "FOR STATEMENT"] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
                 {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("a statement trigger has no SQLite form");
            assert!(
                err.to_string().contains("once per statement"),
                "the error must say what differs, got: {err}"
            );
        }
    }

    /// PostgreSQL defaults to `FOR EACH STATEMENT` when the clause is omitted,
    /// measured on PostgreSQL 16: a trigger written without it fires once for a
    /// three row insert and `information_schema.triggers` reports `STATEMENT`.
    /// SQLite defaults to the opposite, so the omitted spelling must be
    /// rejected too, or the body silently starts running once per row.
    #[test]
    fn a_trigger_without_a_for_each_clause_is_rejected() {
        let trigger = parse_trigger(
            "CREATE TRIGGER docs_ai AFTER INSERT ON docs EXECUTE FUNCTION docs_trigger_fn()",
        );
        let err = trigger
            .translate(&schema_with_trigger_function_and_rls_table(), &Pg2SqliteOptions::default())
            .expect_err("an omitted clause means STATEMENT in PostgreSQL");
        assert!(
            err.to_string().contains("once per statement"),
            "the error must say what differs, got: {err}"
        );
    }

    /// Both row spellings still translate. Guards the rejection from widening
    /// to the case SQLite does support.
    #[test]
    fn row_triggers_still_translate() {
        for spelling in ["FOR EACH ROW", "FOR ROW"] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_ai AFTER INSERT ON docs \
                 {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .unwrap_or_else(|error| panic!("{spelling} is what SQLite does: {error}"));
        }
    }

    /// A transition table gives the body every row the statement touched, so a
    /// SQLite row trigger cannot stand in for it even when the PostgreSQL
    /// trigger is itself `FOR EACH ROW`. Measured on PostgreSQL 16: an
    /// `AFTER INSERT ... REFERENCING NEW TABLE AS nt FOR EACH ROW` trigger over
    /// a three row insert fires three times and sees all three rows each time.
    ///
    /// Each spelling is paired with an event PostgreSQL accepts it on, so the
    /// rejection is the translator's and not a stand-in for an invalid input.
    #[test]
    fn transition_tables_are_rejected() {
        for (event, referencing) in
            [("INSERT", "REFERENCING NEW TABLE AS nt"), ("DELETE", "REFERENCING OLD TABLE AS ot")]
        {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_a AFTER {event} ON docs \
                 {referencing} FOR EACH ROW EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("SQLite has no transition tables");
            assert!(
                err.to_string().contains("transition table"),
                "the error must name the clause, got: {err}"
            );
        }
    }

    /// Every TRUNCATE trigger PostgreSQL accepts is a statement trigger, which
    /// R27 already refuses, but with advice that is wrong here: rewriting it
    /// `FOR EACH ROW` is itself refused by PostgreSQL. So the message has to
    /// name TRUNCATE for every spelling.
    ///
    /// The last case is not valid PostgreSQL and is included because sqlparser
    /// parses it, so it reaches the translator regardless.
    #[test]
    fn truncate_triggers_are_rejected() {
        for spelling in [
            "AFTER TRUNCATE ON docs FOR EACH STATEMENT",
            "AFTER TRUNCATE ON docs",
            "AFTER INSERT OR TRUNCATE ON docs FOR EACH STATEMENT",
            "AFTER TRUNCATE ON docs FOR EACH ROW",
        ] {
            let trigger = parse_trigger(&format!(
                "CREATE TRIGGER docs_at {spelling} EXECUTE FUNCTION docs_trigger_fn()"
            ));
            let err = trigger
                .translate(
                    &schema_with_trigger_function_and_rls_table(),
                    &Pg2SqliteOptions::default(),
                )
                .expect_err("SQLite has no TRUNCATE");
            assert!(
                err.to_string().contains("TRUNCATE"),
                "`{spelling}` must be refused for its event, got: {err}"
            );
        }
    }
}
