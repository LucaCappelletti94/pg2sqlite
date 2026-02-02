//! Preprocessor for PL/pgSQL function bodies.
//!
//! This module transforms PL/pgSQL-specific syntax into forms that
//! can be parsed by sqlparser.
//!
//! ## Transformations
//!
//! 1. **Variable assignments**: `variable := expression;` → `SET variable =
//!    expression;`
//! 2. **DECLARE extraction**: Parses DECLARE block separately to extract
//!    variable info
//! 3. **SELECT INTO**: `SELECT cols INTO vars FROM ...` → preserved for later
//!    handling

use std::fmt::Write;

use super::context::{PlPgSqlContext, VariableBinding, VariableDeclaration};

/// Preprocessor for PL/pgSQL function bodies.
pub struct PlPgSqlPreprocessor;

impl PlPgSqlPreprocessor {
    /// Preprocesses a PL/pgSQL function body string.
    ///
    /// Returns:
    /// - The transformed body ready for parsing
    /// - A context with extracted variable declarations
    ///
    /// # Arguments
    /// * `body` - The raw function body string (including
    ///   DECLARE...BEGIN...END)
    #[must_use]
    pub fn preprocess(body: &str) -> (String, PlPgSqlContext) {
        let mut context = PlPgSqlContext::new();

        // Extract DECLARE block if present
        let (declare_section, body_section) = Self::split_declare_and_body(body);

        // Parse variable declarations
        if let Some(declare) = declare_section {
            Self::parse_declarations(&declare, &mut context);
        }

        // Transform the body
        let transformed = Self::transform_body(&body_section, &context);

        (transformed, context)
    }

    /// Splits the function body into DECLARE section and BEGIN...END section.
    fn split_declare_and_body(body: &str) -> (Option<String>, String) {
        let body_upper = body.to_uppercase();

        // Find DECLARE and BEGIN positions
        let declare_pos = body_upper.find("DECLARE");
        let begin_pos = body_upper.find("BEGIN");

        match (declare_pos, begin_pos) {
            (Some(d), Some(b)) if d < b => {
                // Has DECLARE block before BEGIN
                let declare_section = body[d + 7..b].trim().to_string();
                let body_section = body[b..].to_string();
                (Some(declare_section), body_section)
            }
            (_, Some(b)) => {
                // No DECLARE or DECLARE after BEGIN (unusual)
                (None, body[b..].to_string())
            }
            _ => {
                // No BEGIN found, return as-is
                (None, body.to_string())
            }
        }
    }

    /// Parses variable declarations from a DECLARE section.
    fn parse_declarations(declare_section: &str, context: &mut PlPgSqlContext) {
        // Split by semicolons to get individual declarations
        for decl in declare_section.split(';') {
            let decl = decl.trim();
            if decl.is_empty() {
                continue;
            }

            // Parse: variable_name TYPE [DEFAULT|:= expr]
            if let Some(var_decl) = Self::parse_single_declaration(decl) {
                context.add_declaration(var_decl);
            }
        }
    }

    /// Parses a single variable declaration.
    fn parse_single_declaration(decl: &str) -> Option<VariableDeclaration> {
        // Handle: "v_name UUID" or "v_name UUID := default_value" or "v_name UUID
        // DEFAULT value"
        let decl = decl.trim();
        if decl.is_empty() {
            return None;
        }

        // Check for DEFAULT or :=
        let (name_type, default_value) = if let Some(pos) = decl.find(":=") {
            (decl[..pos].trim(), Some(decl[pos + 2..].trim().to_string()))
        } else if let Some(pos) = decl.to_uppercase().find(" DEFAULT ") {
            (decl[..pos].trim(), Some(decl[pos + 9..].trim().to_string()))
        } else {
            (decl, None)
        };

        // Split name and type
        let parts: Vec<&str> = name_type.split_whitespace().collect();
        if parts.len() >= 2 {
            Some(VariableDeclaration {
                name: parts[0].to_string(),
                data_type: parts[1..].join(" "),
                default_value,
            })
        } else {
            None
        }
    }

    /// Transforms the body section, converting PL/pgSQL syntax to parseable
    /// SQL.
    fn transform_body(body: &str, context: &PlPgSqlContext) -> String {
        let mut result = body.to_string();

        // Transform variable assignments: `variable := expression;` → `SET variable =
        // expression;` We need to be careful to only transform standalone
        // assignments, not within expressions
        result = Self::transform_assignments(&result, context);

        // Transform SELECT INTO statements to extract variable bindings
        result = Self::transform_select_into(&result, context);

        result
    }

    /// Transforms SELECT ... INTO var1, var2 FROM ... statements.
    ///
    /// PL/pgSQL: SELECT col1, col2 INTO v1, v2 FROM table WHERE ...
    /// We convert this to a form that records the bindings and removes the INTO
    /// clause since `SQLite` doesn't support SELECT INTO in triggers.
    fn transform_select_into(body: &str, context: &PlPgSqlContext) -> String {
        // We need to find SELECT ... INTO ... FROM patterns
        // and transform them into SET statements that can be parsed
        let mut result = String::new();
        let chars = body.chars().peekable();
        let mut current_stmt = String::new();
        let mut in_string = false;
        let mut string_char = ' ';

        for c in chars {
            // Track string literals
            if (c == '\'' || c == '"') && !in_string {
                in_string = true;
                string_char = c;
            } else if c == string_char && in_string {
                in_string = false;
            }

            current_stmt.push(c);

            // Check for statement end (semicolon not in string)
            if c == ';' && !in_string {
                // Process the statement
                let transformed = Self::try_transform_select_into_stmt(&current_stmt, context);
                result.push_str(&transformed);
                current_stmt.clear();
            }
        }

        // Handle any remaining content
        if !current_stmt.is_empty() {
            result.push_str(&current_stmt);
        }

        result
    }

    /// Tries to transform a single SELECT INTO statement.
    fn try_transform_select_into_stmt(stmt: &str, _context: &PlPgSqlContext) -> String {
        let stmt_upper = stmt.to_uppercase();

        // Check if this is a SELECT ... INTO ... FROM statement
        let select_pos = stmt_upper.find("SELECT");
        let into_pos = stmt_upper.find(" INTO ");

        // Find FROM that comes AFTER INTO (not before, which could be in comments)
        let from_pos = if let Some(i) = into_pos {
            stmt_upper[i..].find(" FROM ").map(|f| f + i)
        } else {
            None
        };

        // Must have SELECT, INTO, FROM in that order
        match (select_pos, into_pos, from_pos) {
            (Some(s), Some(i), Some(f)) if s < i && i < f => {
                // Preserve any content before SELECT (like BEGIN, comments, whitespace)
                let prefix = &stmt[..s];

                // Extract parts:
                // - columns: between SELECT and INTO
                // - variables: between INTO and FROM
                // - rest: FROM onwards

                let columns_part = stmt[s + 6..i].trim();
                let vars_part = stmt[i + 6..f].trim();
                let from_part = &stmt[f..];

                // Parse column names
                let columns: Vec<&str> =
                    columns_part.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();

                // Parse variable names
                let vars: Vec<&str> =
                    vars_part.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();

                if columns.len() != vars.len() || columns.is_empty() {
                    // Can't transform, return as-is
                    return stmt.to_string();
                }

                // Generate SET statements for each variable
                // SET var = (SELECT col FROM ... LIMIT 1);
                let mut result = String::new();

                // Preserve prefix (like BEGIN)
                result.push_str(prefix);

                // Get indentation from SELECT position
                let select_line_start = stmt[..s].rfind('\n').map_or(0, |p| p + 1);
                let indent = &stmt[select_line_start..s];

                for (i, (col, var)) in columns.iter().zip(vars.iter()).enumerate() {
                    // Build a subquery for each variable
                    let subquery = format!("(SELECT {col}{}", from_part.trim_end_matches(';'));
                    // Add LIMIT 1 if not present
                    let subquery = if subquery.to_uppercase().contains(" LIMIT ") {
                        format!("{subquery})")
                    } else {
                        format!("{subquery} LIMIT 1)")
                    };

                    if i > 0 {
                        result.push_str(indent);
                    }
                    let _ = write!(result, "SET {var} = {subquery};");
                    if i < columns.len() - 1 {
                        result.push('\n');
                    }
                }

                result
            }
            _ => stmt.to_string(),
        }
    }

    /// Transforms variable assignments from := to SET syntax.
    fn transform_assignments(body: &str, context: &PlPgSqlContext) -> String {
        let mut result = String::new();
        let lines: Vec<&str> = body.lines().collect();

        for line in lines {
            let trimmed = line.trim();

            // Check if this line is a simple variable assignment
            if let Some(assignment) = Self::parse_assignment_line(trimmed, context) {
                // Replace with SET syntax
                let indent = &line[..line.len() - line.trim_start().len()];
                result.push_str(indent);
                let _ = write!(result, "SET {} = {};", assignment.name, assignment.expression);
                result.push('\n');
            } else {
                result.push_str(line);
                result.push('\n');
            }
        }

        result
    }

    /// Parses a line to see if it's a variable assignment.
    fn parse_assignment_line(line: &str, _context: &PlPgSqlContext) -> Option<VariableBinding> {
        // Remove trailing semicolon
        let line = line.trim().trim_end_matches(';').trim();

        // Look for :=
        let assign_pos = line.find(":=")?;

        let var_name = line[..assign_pos].trim();
        let expression = line[assign_pos + 2..].trim();

        // Verify it's a declared variable (or looks like one - starts with letter,
        // contains only valid chars)
        let is_valid_var = var_name.chars().all(|c| c.is_alphanumeric() || c == '_')
            && var_name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');

        if is_valid_var && !expression.is_empty() {
            Some(VariableBinding { name: var_name.to_string(), expression: expression.to_string() })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_declare_and_body() {
        let body = r"
DECLARE
    v_id UUID;
    v_name TEXT;
BEGIN
    v_id := uuidv7();
    INSERT INTO t (id) VALUES (v_id);
END
";

        let (declare, body) = PlPgSqlPreprocessor::split_declare_and_body(body);
        assert!(declare.is_some());
        assert!(declare.unwrap().contains("v_id UUID"));
        assert!(body.contains("BEGIN"));
    }

    #[test]
    fn test_parse_declaration() {
        let decl = PlPgSqlPreprocessor::parse_single_declaration("v_new_id UUID");
        assert!(decl.is_some());
        let decl = decl.unwrap();
        assert_eq!(decl.name, "v_new_id");
        assert_eq!(decl.data_type, "UUID");
        assert!(decl.default_value.is_none());
    }

    #[test]
    fn test_parse_declaration_with_default() {
        let decl = PlPgSqlPreprocessor::parse_single_declaration("v_count INT DEFAULT 0");
        assert!(decl.is_some());
        let decl = decl.unwrap();
        assert_eq!(decl.name, "v_count");
        assert_eq!(decl.data_type, "INT");
        assert_eq!(decl.default_value, Some("0".to_string()));
    }

    #[test]
    fn test_transform_select_into() {
        let body = r"BEGIN
    SELECT o.owner_id, o.creator_id, e.role_level
    INTO v_owner_id, v_creator_id, v_role_level
    FROM ownables o
    JOIN entities e ON o.id = e.id
    WHERE o.id = NEW.id;
END";

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body);
        println!("Transformed:\n{transformed}");

        // Should have SET statements instead of SELECT INTO
        assert!(transformed.contains("SET v_owner_id ="), "Should transform v_owner_id");
        assert!(transformed.contains("SET v_creator_id ="), "Should transform v_creator_id");
        assert!(transformed.contains("SET v_role_level ="), "Should transform v_role_level");
        assert!(!transformed.contains("INTO v_owner_id"), "Should remove INTO clause");
    }

    #[test]
    fn test_transform_select_into_with_declare() {
        let body = r"DECLARE
    v_new_id UUID;
    v_owner_id UUID;
BEGIN
    SELECT o.owner_id, o.creator_id
    INTO v_owner_id, v_creator_id
    FROM ownables o
    WHERE o.id = NEW.id;
END";

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body);
        println!("Transformed with DECLARE:\n{transformed}");

        // Should have SET statements instead of SELECT INTO
        assert!(transformed.contains("SET v_owner_id ="), "Should transform v_owner_id");
        assert!(transformed.contains("SET v_creator_id ="), "Should transform v_creator_id");
        assert!(!transformed.contains("INTO v_owner_id"), "Should remove INTO clause");
    }
}
