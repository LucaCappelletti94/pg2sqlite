//! Submodule with the implementations of the [`crate::traits::Translator`]
//! trait.

mod column;
mod column_option;
mod constraint_characteristic;
mod create_index;
mod create_table;
mod create_trigger;
mod create_view;
mod data_type;
mod delete;
mod expr;
mod function;
mod index_column;
mod insert;
mod order_by_expr;
pub mod plpgsql;
mod query;
mod referential_action;
pub mod rls;
mod statement;
mod table_constraint;
