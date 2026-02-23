//! Coverage-focused tests for small translator impl modules.

use pg2sqlite::{errors::Error, options::Pg2SqliteOptions, prelude::Translator};
use sql_traits::structs::ParserDB;
use sqlparser::ast::{
    ConstraintCharacteristics, DeferrableInitial, Expr, Ident, OrderByExpr, OrderByOptions,
    ReferentialAction, Value, ValueWithSpan, WithFill,
};

fn empty_schema() -> ParserDB {
    ParserDB::from_statements(Vec::new(), "test".to_string()).expect("schema should build")
}

#[test]
fn constraint_characteristics_translation_is_rejected() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let characteristics = ConstraintCharacteristics {
        deferrable: Some(true),
        initially: Some(DeferrableInitial::Deferred),
        enforced: Some(false),
    };

    let error = characteristics
        .translate(&schema, &options)
        .expect_err("constraint characteristics are unsupported in SQLite translation");
    assert!(matches!(
        error,
        Error::UnsupportedSQLiteFeature(message)
            if message.contains("Unsupported constraint characteristic")
    ));
}

#[test]
fn referential_action_translation_passthrough_covers_all_variants() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();

    for action in [
        ReferentialAction::NoAction,
        ReferentialAction::Restrict,
        ReferentialAction::SetNull,
        ReferentialAction::SetDefault,
        ReferentialAction::Cascade,
    ] {
        let translated = action
            .translate(&schema, &options)
            .expect("referential actions should translate as-is");
        assert_eq!(translated, action);
    }
}

#[test]
fn order_by_expr_translation_rejects_with_fill_clause() {
    let schema = empty_schema();
    let options = Pg2SqliteOptions::default();
    let order_by_expr = OrderByExpr {
        expr: Expr::Identifier(Ident::new("created_at")),
        options: OrderByOptions::default(),
        with_fill: Some(WithFill {
            from: Some(Expr::Value(ValueWithSpan::from(Value::Number("1".to_string(), false)))),
            to: None,
            step: None,
        }),
    };

    let error =
        order_by_expr.translate(&schema, &options).expect_err("WITH FILL should not be supported");
    assert!(matches!(
        error,
        Error::UnknownPostgresFeature(message) if message == "WITH FILL in ORDER BY"
    ));
}
