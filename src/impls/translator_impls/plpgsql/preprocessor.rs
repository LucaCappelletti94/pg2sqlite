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

        // Transform PostgreSQL ELSIF → ELSEIF (sqlparser uses ELSEIF keyword)
        result = Self::transform_elsif(&result);

        // Transform variable assignments: `variable := expression;` → `SET variable =
        // expression;` We need to be careful to only transform standalone
        // assignments, not within expressions
        result = Self::transform_assignments(&result, context);

        // Transform SELECT INTO statements to extract variable bindings
        result = Self::transform_select_into(&result, context);

        // Transform RAISE EXCEPTION/NOTICE/... → SELECT RAISE(ABORT, ...) or drop
        result = Self::transform_raise_statements(&result);

        result
    }

    /// Transforms PL/pgSQL `RAISE` statements into SQLite-compatible form.
    ///
    /// Scans the body text directly (not statement-by-statement) so that RAISE
    /// inside IF/THEN blocks is also rewritten.
    ///
    /// - `RAISE EXCEPTION 'msg'` → `SELECT RAISE(ABORT, 'msg')`
    /// - `RAISE EXCEPTION 'fmt %', arg` → `SELECT RAISE(ABORT, 'fmt %')`
    ///   (format args dropped)
    /// - `RAISE NOTICE/INFO/WARNING/DEBUG/LOG ...` → removed (no SQLite
    ///   equivalent)
    fn transform_raise_statements(body: &str) -> String {
        let mut result = String::new();
        let mut search_from = 0;
        let upper_body = body.to_uppercase();

        while let Some(raise_rel) = upper_body[search_from..].find("RAISE ") {
            let raise_pos = search_from + raise_rel;

            // Append everything up to and including the RAISE keyword
            // (we'll decide below whether to keep or replace it)
            let after_raise_start = raise_pos + 6; // byte offset after "RAISE "

            let rest_upper = &upper_body[after_raise_start..];

            let (level_bytes, is_exception) = if rest_upper.starts_with("EXCEPTION") {
                (9usize, true)
            } else if rest_upper.starts_with("NOTICE")
                || rest_upper.starts_with("WARNING")
                || rest_upper.starts_with("INFO ")
                || rest_upper.starts_with("INFO\n")
                || rest_upper.starts_with("INFO;")
                || rest_upper.starts_with("DEBUG")
                || rest_upper.starts_with("LOG")
            {
                let len = rest_upper
                    .find(|c: char| c.is_whitespace() || c == ';')
                    .unwrap_or(rest_upper.len());
                (len, false)
            } else {
                // Not a recognized RAISE level - pass through unchanged
                result.push_str(&body[search_from..after_raise_start]);
                search_from = after_raise_start;
                continue;
            };

            // Emit everything before this RAISE
            result.push_str(&body[search_from..raise_pos]);

            // Find the semicolon that ends this RAISE statement
            let after_level = after_raise_start + level_bytes;
            let stmt_end = Self::find_unquoted_semicolon(&body[after_level..])
                .map_or(body.len().saturating_sub(1), |p| after_level + p);

            if is_exception {
                let message_content = body[after_level..stmt_end].trim();
                let msg = Self::extract_first_string_literal(message_content);
                result.push_str("SELECT RAISE(ABORT, ");
                result.push_str(msg);
                result.push(')');
                // Keep the semicolon
                result.push(';');
            }
            // else: informational - drop entirely (emit nothing)

            search_from = stmt_end + 1; // skip past the semicolon
        }

        result.push_str(&body[search_from..]);
        result
    }

    /// Returns the byte offset of the first unquoted semicolon in `s`.
    fn find_unquoted_semicolon(s: &str) -> Option<usize> {
        let mut in_string = false;
        let mut string_char = ' ';
        for (i, c) in s.char_indices() {
            if in_string {
                if c == string_char {
                    in_string = false;
                }
            } else if c == '\'' || c == '"' {
                in_string = true;
                string_char = c;
            } else if c == ';' {
                return Some(i);
            }
        }
        None
    }

    /// Extracts the first string literal from a comma-separated argument list.
    ///
    /// For `'hello', arg1, arg2` returns `'hello'`.
    /// If the first argument is not a string literal, returns `'error'` as a
    /// safe fallback.
    fn extract_first_string_literal(args: &str) -> &str {
        let trimmed = args.trim();
        if let Some(stripped) = trimmed.strip_prefix('\'') {
            // Find the closing quote, respecting escaped '' pairs
            let mut chars = stripped.char_indices();
            while let Some((i, c)) = chars.next() {
                if c == '\'' {
                    // Check for '' escape
                    match chars.next() {
                        Some((_, '\'')) => {}          // escaped quote, keep going
                        _ => return &trimmed[..i + 2], // +1 for opening quote, +1 for closing
                    }
                }
            }
        }
        // Fallback: take up to first comma (or whole thing)
        match args.find(',') {
            Some(p) => args[..p].trim(),
            None => args.trim(),
        }
    }

    /// Transforms PostgreSQL ELSIF keyword to ELSEIF (which sqlparser expects).
    fn transform_elsif(body: &str) -> String {
        // Use case-insensitive replacement
        // We need to be careful not to replace ELSIF inside strings
        let mut result = String::new();
        let mut chars = body.chars().peekable();
        let mut in_string = false;
        let mut string_char = ' ';

        while let Some(c) = chars.next() {
            // Track string literals
            if (c == '\'' || c == '"') && !in_string {
                in_string = true;
                string_char = c;
                result.push(c);
                continue;
            } else if c == string_char && in_string {
                in_string = false;
                result.push(c);
                continue;
            }

            if in_string {
                result.push(c);
                continue;
            }

            // Check for ELSIF (case-insensitive)
            if c.eq_ignore_ascii_case(&'E') {
                let mut word = String::from(c);
                let remaining = ['L', 'S', 'I', 'F'];
                let mut matched = true;

                for expected in remaining {
                    if let Some(&next) = chars.peek() {
                        if next.eq_ignore_ascii_case(&expected) {
                            word.push(chars.next().unwrap());
                        } else {
                            matched = false;
                            break;
                        }
                    } else {
                        matched = false;
                        break;
                    }
                }

                if matched && word.to_uppercase() == "ELSIF" {
                    // Check that next char is whitespace or end (not part of larger word)
                    if chars.peek().is_none_or(|&c| !c.is_alphanumeric() && c != '_') {
                        result.push_str("ELSEIF");
                    } else {
                        result.push_str(&word);
                    }
                } else {
                    result.push_str(&word);
                }
            } else {
                result.push(c);
            }
        }

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
        let chars = body.chars();
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
        let from_pos = into_pos.and_then(|i| stmt_upper[i..].find(" FROM ").map(|f| f + i));

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
                let columns = Self::split_top_level_csv(columns_part);

                // Parse variable names
                let vars = Self::split_top_level_csv(vars_part);

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

    /// Splits a comma-separated SQL fragment on top-level commas only.
    ///
    /// Commas inside parentheses or quoted strings are ignored.
    fn split_top_level_csv(input: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut start = 0usize;
        let mut paren_depth = 0usize;
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut chars = input.char_indices().peekable();

        while let Some((idx, ch)) = chars.next() {
            if in_single_quote {
                if ch == '\'' {
                    if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                        // Escaped quote in SQL string literal ('')
                        chars.next();
                    } else {
                        in_single_quote = false;
                    }
                }
                continue;
            }

            if in_double_quote {
                if ch == '"' {
                    in_double_quote = false;
                }
                continue;
            }

            match ch {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                '(' => paren_depth += 1,
                ')' => paren_depth = paren_depth.saturating_sub(1),
                ',' if paren_depth == 0 => {
                    let part = input[start..idx].trim();
                    if !part.is_empty() {
                        parts.push(part);
                    }
                    start = idx + 1;
                }
                _ => {}
            }
        }

        let tail = input[start..].trim();
        if !tail.is_empty() {
            parts.push(tail);
        }

        parts
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
            } else {
                result.push_str(line);
            }
            result.push('\n');
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

    #[test]
    fn test_transform_select_into_with_comma_in_expression() {
        let body = r"BEGIN
    SELECT COALESCE(NEW.a, NEW.b)
    INTO v_value
    FROM t
    LIMIT 1;
END";

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body);

        assert!(transformed.contains("SET v_value ="), "Should transform INTO target");
        assert!(
            transformed.contains("COALESCE(NEW.a, NEW.b)"),
            "Should preserve expression with comma"
        );
        assert!(!transformed.contains(" INTO "), "Should remove INTO clause");
    }

    #[test]
    fn test_parse_declaration_and_assignment_edge_cases() {
        assert!(PlPgSqlPreprocessor::parse_single_declaration("").is_none());
        assert!(PlPgSqlPreprocessor::parse_single_declaration("only_name").is_none());

        let decl = PlPgSqlPreprocessor::parse_single_declaration("v_now TIMESTAMP := now()");
        assert!(decl.is_some());
        let decl = decl.unwrap();
        assert_eq!(decl.name, "v_now");
        assert_eq!(decl.data_type, "TIMESTAMP");
        assert_eq!(decl.default_value.as_deref(), Some("now()"));

        let context = PlPgSqlContext::new();
        assert!(PlPgSqlPreprocessor::parse_assignment_line("1bad := 1;", &context).is_none());
        assert!(PlPgSqlPreprocessor::parse_assignment_line("v_ok := ;", &context).is_none());
    }

    #[test]
    fn test_transform_elsif_handles_partial_and_identifier_suffix_inputs() {
        let single = PlPgSqlPreprocessor::transform_elsif("E");
        assert_eq!(single, "E");

        let transformed = PlPgSqlPreprocessor::transform_elsif("E\nELS\nELSIFX\nELSIF\n");
        assert!(transformed.contains("E\n"));
        assert!(transformed.contains("ELS\n"));
        assert!(transformed.contains("ELSIFX\n"));
        assert!(transformed.contains("ELSEIF\n"));
    }

    #[test]
    fn test_transform_raise_exception_simple() {
        let body = "BEGIN\n  RAISE EXCEPTION 'val must be non-negative';\n  RETURN NEW;\nEND";
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body);
        assert!(
            transformed.contains("SELECT RAISE(ABORT, 'val must be non-negative')"),
            "RAISE EXCEPTION should become SELECT RAISE(ABORT, ...), got: {transformed}"
        );
        assert!(!transformed.contains("RAISE EXCEPTION"), "Original form should be removed");
    }

    #[test]
    fn test_transform_raise_exception_with_format_args() {
        let body = "BEGIN\n  RAISE EXCEPTION 'bad value: %', NEW.val;\n  RETURN NEW;\nEND";
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body);
        assert!(
            transformed.contains("SELECT RAISE(ABORT, 'bad value: %')"),
            "Format args should be dropped, got: {transformed}"
        );
    }

    #[test]
    fn test_transform_raise_notice_dropped() {
        let body = "BEGIN\n  RAISE NOTICE 'debug info';\n  RETURN NEW;\nEND";
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body);
        assert!(
            !transformed.contains("RAISE NOTICE"),
            "RAISE NOTICE should be dropped, got: {transformed}"
        );
    }

    #[test]
    fn test_split_top_level_csv_handles_escaped_single_and_double_quotes() {
        let parts = PlPgSqlPreprocessor::split_top_level_csv(
            "'a''b', \"x,y\", COALESCE(a, b), plain_value",
        );
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "'a''b'");
        assert_eq!(parts[1], "\"x,y\"");
        assert_eq!(parts[2], "COALESCE(a, b)");
        assert_eq!(parts[3], "plain_value");
    }
}
