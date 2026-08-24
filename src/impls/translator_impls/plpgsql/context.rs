//! Context tracking for PL/pgSQL translation.
//!
//! This module defines the `PlPgSqlContext` struct which tracks variable
//! declarations, their types, and the current translation scope.

use alloc::collections::BTreeMap;
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

/// Represents a variable declaration from a PL/pgSQL DECLARE block.
#[derive(Debug, Clone)]
pub struct VariableDeclaration {
    /// The variable name (e.g., "`v_new_id`")
    pub name: String,
    /// The declared type as a string (e.g., "UUID", "SMALLINT")
    pub data_type: String,
    /// Default value expression, if any
    pub default_value: Option<String>,
}

/// Represents a variable binding (assignment) within a block.
#[derive(Debug, Clone)]
pub struct VariableBinding {
    /// The variable name
    pub name: String,
    /// The expression string that computes the value
    pub expression: String,
}

/// Tracks the first INSERT that used a UUID variable.
/// This allows subsequent INSERTs to use `last_insert_rowid()` to get the same
/// value.
#[derive(Debug, Clone)]
pub struct UuidFirstUse {
    /// The table name where the UUID was first inserted
    pub table_name: String,
    /// The column name containing the UUID
    pub column_name: String,
}

/// Translation context for PL/pgSQL to SQLite. Tracks scoped bindings (cleared
/// per IF block) separately from persistent ones (SELECT INTO), plus UUID
/// first-use state for the `last_insert_rowid()` pattern.
#[derive(Debug, Clone, Default)]
pub struct PlPgSqlContext {
    declarations: BTreeMap<String, VariableDeclaration>,
    persistent_bindings: BTreeMap<String, VariableBinding>,
    scoped_bindings: BTreeMap<String, VariableBinding>,
    condition_stack: Vec<String>,
    uuid_first_use: BTreeMap<String, UuidFirstUse>,
    /// The event names (INSERT, UPDATE, DELETE) the emitted trigger fires on.
    /// Empty when the context is not inside a trigger body.
    pub trigger_events: Vec<String>,
    /// The table the trigger fires on, or `None` when not inside a trigger
    /// body.
    pub trigger_table: Option<String>,
}

impl PlPgSqlContext {
    /// Creates a new empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a variable declaration to the context.
    pub fn add_declaration(&mut self, decl: VariableDeclaration) {
        self.declarations.insert(decl.name.clone(), decl);
    }

    /// Gets a variable declaration by name.
    #[must_use]
    pub fn get_declaration(&self, name: &str) -> Option<&VariableDeclaration> {
        self.declarations.get(name)
    }

    /// Checks if a name is a declared variable.
    #[must_use]
    pub fn is_declared_variable(&self, name: &str) -> bool {
        self.declarations.contains_key(name)
    }

    /// Adds a persistent variable binding (e.g., from SELECT INTO).
    /// These persist across IF block boundaries.
    pub fn add_persistent_binding(&mut self, binding: VariableBinding) {
        self.persistent_bindings.insert(binding.name.clone(), binding);
    }

    /// Adds a scoped variable binding (assignment in IF block).
    /// These are cleared when entering a new IF scope.
    pub fn add_binding(&mut self, binding: VariableBinding) {
        self.scoped_bindings.insert(binding.name.clone(), binding);
    }

    /// Gets a variable binding by name (checks scoped first, then persistent).
    #[must_use]
    pub fn get_binding(&self, name: &str) -> Option<&VariableBinding> {
        self.scoped_bindings.get(name).or_else(|| self.persistent_bindings.get(name))
    }

    /// Returns all current bindings (both scoped and persistent).
    pub fn bindings(&self) -> impl Iterator<Item = &VariableBinding> {
        self.scoped_bindings.values().chain(self.persistent_bindings.values())
    }

    /// Clears scoped bindings (used when entering a new IF scope).
    /// Persistent bindings are kept.
    pub fn clear_scoped_bindings(&mut self) {
        self.scoped_bindings.clear();
    }

    /// Pushes a condition onto the condition stack (entering an IF block).
    pub fn push_condition(&mut self, condition: String) {
        self.condition_stack.push(condition);
    }

    /// Pops a condition from the stack (exiting an IF block).
    pub fn pop_condition(&mut self) -> Option<String> {
        self.condition_stack.pop()
    }

    /// Gets the current combined condition (AND of all stacked conditions).
    #[must_use]
    pub fn current_condition(&self) -> Option<String> {
        if self.condition_stack.is_empty() {
            None
        } else {
            Some(
                self.condition_stack
                    .iter()
                    .map(|c| format!("({c})"))
                    .collect::<Vec<_>>()
                    .join(" AND "),
            )
        }
    }

    /// Checks if a binding is a UUID generation expression (`uuidv7()`,
    /// `gen_random_uuid()`, etc.)
    #[must_use]
    pub fn is_uuid_generation(&self, name: &str) -> bool {
        self.get_binding(name).is_some_and(|binding| {
            let expr_lower = binding.expression.to_lowercase();
            expr_lower.contains("uuidv7()")
                || expr_lower.contains("uuidv4()")
                || expr_lower.contains("uuid_generate_v4()")
                || expr_lower.contains("gen_random_uuid()")
        })
    }

    /// Records that a UUID variable was first used in an INSERT to a specific
    /// table/column.
    pub fn record_uuid_first_use(&mut self, var_name: &str, table_name: &str, column_name: &str) {
        self.uuid_first_use.insert(
            var_name.to_string(),
            UuidFirstUse {
                table_name: table_name.to_string(),
                column_name: column_name.to_string(),
            },
        );
    }

    /// Gets the first use info for a UUID variable (if it was already used).
    #[must_use]
    pub fn get_uuid_first_use(&self, var_name: &str) -> Option<&UuidFirstUse> {
        self.uuid_first_use.get(var_name)
    }

    /// Clears UUID first-use tracking (e.g., when entering a new IF block where
    /// a new UUID is generated).
    pub fn clear_uuid_first_use(&mut self) {
        self.uuid_first_use.clear();
    }

    /// Seeds persistent bindings from declaration defaults.
    ///
    /// Declarations are function-scoped and should remain visible across IF
    /// block boundaries, so defaults are initialized as persistent bindings.
    pub fn seed_default_bindings(&mut self) {
        let defaults = self
            .declarations
            .values()
            .filter_map(|decl| {
                decl.default_value.as_ref().map(|default_value| {
                    VariableBinding { name: decl.name.clone(), expression: default_value.clone() }
                })
            })
            .collect::<Vec<_>>();

        for binding in defaults {
            self.persistent_bindings.entry(binding.name.clone()).or_insert(binding);
        }
    }

    /// Returns the event string for TG_OP constant-folding when there is
    /// exactly one trigger event, otherwise `None`.
    ///
    /// A multi-event trigger cannot fold TG_OP to a single value.
    #[must_use]
    pub fn single_trigger_event(&self) -> Option<&str> {
        if self.trigger_events.len() == 1 { Some(&self.trigger_events[0]) } else { None }
    }

    /// True when the context has more than one trigger event and TG_OP cannot
    /// be resolved to a single value.
    #[must_use]
    pub fn has_multiple_trigger_events(&self) -> bool {
        self.trigger_events.len() > 1
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::{PlPgSqlContext, VariableBinding, VariableDeclaration};

    #[test]
    fn declarations_and_bindings_are_tracked() {
        let mut ctx = PlPgSqlContext::new();

        ctx.add_declaration(VariableDeclaration {
            name: "v_id".to_string(),
            data_type: "UUID".to_string(),
            default_value: Some("uuidv7()".to_string()),
        });
        assert!(ctx.is_declared_variable("v_id"));
        assert_eq!(ctx.get_declaration("v_id").map(|d| d.data_type.as_str()), Some("UUID"));

        ctx.add_persistent_binding(VariableBinding {
            name: "v_persist".to_string(),
            expression: "(SELECT NEW.id)".to_string(),
        });
        ctx.add_binding(VariableBinding {
            name: "v_scoped".to_string(),
            expression: "42".to_string(),
        });

        assert_eq!(
            ctx.get_binding("v_persist").map(|b| b.expression.as_str()),
            Some("(SELECT NEW.id)")
        );
        assert_eq!(ctx.get_binding("v_scoped").map(|b| b.expression.as_str()), Some("42"));

        let binding_names = ctx.bindings().map(|b| b.name.clone()).collect::<Vec<_>>();
        assert!(binding_names.iter().any(|n| n == "v_persist"));
        assert!(binding_names.iter().any(|n| n == "v_scoped"));

        ctx.clear_scoped_bindings();
        assert!(ctx.get_binding("v_scoped").is_none());
        assert!(ctx.get_binding("v_persist").is_some());
    }

    #[test]
    fn conditions_and_uuid_tracking_are_scoped() {
        let mut ctx = PlPgSqlContext::new();

        assert!(ctx.current_condition().is_none());
        ctx.push_condition("NEW.kind = 'a'".to_string());
        ctx.push_condition("NEW.active = true".to_string());
        assert_eq!(
            ctx.current_condition().as_deref(),
            Some("(NEW.kind = 'a') AND (NEW.active = true)")
        );
        assert_eq!(ctx.pop_condition().as_deref(), Some("NEW.active = true"));
        assert_eq!(ctx.current_condition().as_deref(), Some("(NEW.kind = 'a')"));

        ctx.add_binding(VariableBinding {
            name: "v_uuid".to_string(),
            expression: "gen_random_uuid()".to_string(),
        });
        assert!(ctx.is_uuid_generation("v_uuid"));

        ctx.record_uuid_first_use("v_uuid", "items", "id");
        let first_use = ctx.get_uuid_first_use("v_uuid").unwrap();
        assert_eq!(first_use.table_name, "items");
        assert_eq!(first_use.column_name, "id");

        ctx.clear_uuid_first_use();
        assert!(ctx.get_uuid_first_use("v_uuid").is_none());
    }
}
