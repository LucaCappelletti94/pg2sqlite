use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, DatabaseLike, TableLike, TriggerLike},
};
use sqlparser::{
    ast::{
        Assignment, AssignmentTarget, BinaryOperator, ConditionalStatements, CreateTrigger,
        DropTrigger, Expr, Ident, ObjectName, ObjectNamePart, Statement, TableFactor,
        TableWithJoins, TriggerExecBodyType, TriggerPeriod, Update,
        helpers::attached_token::AttachedToken,
    },
    keywords::Keyword,
    tokenizer::{Token, TokenWithSpan, Word},
};

use crate::{
    impls::object_name::{append_suffix, schema_and_table_for_lookup},
    options::Pg2SqliteOptions,
    traits::{schema::Schema, translation_options::TranslationOptions, translator::Translator},
};

fn generate_maintenance_trigger_body(
    trigger: &CreateTrigger,
    schema: &ParserDB,
) -> sqlparser::ast::BeginEndStatements {
    let assignments = trigger
        .maintenance_assignments(schema)
        .map(|(col, expr)| {
            Assignment {
                target: AssignmentTarget::ColumnName(ObjectName(vec![ObjectNamePart::Identifier(
                    Ident::new(col.column_name()),
                )])),
                value: expr,
            }
        })
        .collect::<Vec<_>>();

    let update_stmt = Statement::Update(Update {
        update_token: AttachedToken(TokenWithSpan::wrap(Token::Word(Word {
            value: "UPDATE".into(),
            quote_style: None,
            keyword: Keyword::UPDATE,
        }))),
        table: TableWithJoins {
            relation: TableFactor::Table {
                name: trigger.table_name.clone(),
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
            right: Box::new(Expr::CompoundIdentifier(vec![Ident::new("OLD"), Ident::new("rowid")])),
        }),
        returning: None,
        or: None,
        limit: None,
        optimizer_hint: None,
    });

    sqlparser::ast::BeginEndStatements {
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
    }
}

fn generate_standard_trigger_body(
    exec_body: &sqlparser::ast::TriggerExecBody,
    schema: &ParserDB,
    options: &Pg2SqliteOptions,
) -> Result<Option<sqlparser::ast::BeginEndStatements>, crate::errors::Error> {
    let function_name = exec_body.func_desc.name.clone();
    if let Some(mut body) = schema.function_body(&function_name.to_string())? {
        body.statements = super::plpgsql::PlPgSqlTranslator::translate(&body, schema, options)?;
        Ok(Some(body))
    } else {
        Ok(None)
    }
}

impl Translator for CreateTrigger {
    type Schema = ParserDB;
    type Options = Pg2SqliteOptions;
    type SQLiteEntry = Option<(Option<DropTrigger>, Self)>;

    #[allow(clippy::too_many_lines)]
    fn translate(
        &self,
        schema: &Self::Schema,
        options: &Self::Options,
    ) -> Result<Self::SQLiteEntry, crate::errors::Error> {
        let CreateTrigger {
            or_alter,
            temporary,
            or_replace,
            is_constraint,
            name,
            period,
            events,
            table_name,
            referenced_table_name,
            referencing,
            trigger_object,
            period_before_table,
            condition,
            statements_as,
            exec_body,
            statements,
            characteristics,
        } = self.clone();

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

        let function_body = if self.is_maintenance_trigger(schema) {
            generate_maintenance_trigger_body(self, schema)
        } else {
            match generate_standard_trigger_body(&exec_body, schema, options)? {
                Some(body) => body,
                None => return Ok(None),
            }
        };

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

        // For BEFORE/AFTER triggers on RLS-protected tables, redirect to the underlying
        // _rls table. INSTEAD OF triggers are used on the view, but
        // BEFORE/AFTER triggers must target the actual table (which has been
        // renamed to table_rls).
        let redirected_table_name =
            if matches!(period, Some(TriggerPeriod::Before | TriggerPeriod::After)) {
                let (table_schema, table_name_part) = schema_and_table_for_lookup(&table_name);
                if table_name_part
                    .and_then(|name_part| schema.table(table_schema, name_part))
                    .is_some_and(|table| table.has_row_level_security(schema))
                {
                    append_suffix(&table_name, options.get_rls_table_suffix())
                } else {
                    table_name
                }
            } else {
                table_name
            };

        Ok(Some((
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
        )))
    }
}
