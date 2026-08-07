//! Preprocessor for PL/pgSQL function bodies.
//!
//! Transforms variable assignments (`:=`) into SET statements, extracts DECLARE
//! blocks, and rewrites SELECT INTO as SET subqueries for sqlparser
//! compatibility.

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
use core::fmt::Write;

use super::{
    context::{PlPgSqlContext, VariableBinding, VariableDeclaration},
    scanner::Scanner,
};
use crate::errors::Error;

/// Preprocessor for PL/pgSQL function bodies.
pub struct PlPgSqlPreprocessor;

/// Rewrite a dollar-quoted default as a single-quoted one, which is the only
/// string literal SQLite has.
///
/// Anything else is returned unchanged, so a value that is already a literal or
/// an expression passes through.
fn single_quoted_default(default: &str) -> String {
    let Some(body) = dollar_quoted_body(default) else { return default.to_string() };
    format!("'{}'", body.replace('\'', "''"))
}

/// The text between a dollar-quoted string's delimiters, when `text` is exactly
/// one such string.
fn dollar_quoted_body(text: &str) -> Option<&str> {
    let tag_end = text.strip_prefix('$')?.find('$')? + 1;
    let delimiter = &text[..=tag_end];
    if !delimiter[1..tag_end].chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let body = text.get(delimiter.len()..)?.strip_suffix(delimiter)?;
    Some(body)
}

/// Refuses a statement that assigns to a qualified target.
///
/// `NEW.col := expr` means the trigger changes the row being written. That is
/// emulated with an `UPDATE` over the written row, but only for the shape
/// `sql-traits` recognises, a run of such assignments to declared columns
/// closed by `RETURN NEW`. Anywhere else the rewrite below splits the name at
/// the `.` and produces `NEW.SET col = expr`, which is not SQL.
///
/// # Errors
///
/// Returns [`Error::UnsupportedSQLiteFeature`] naming the target.
fn reject_qualified_assignment(statement: &str) -> Result<(), Error> {
    let Some((qualifier, name)) = qualified_assignment_target(statement) else {
        return Ok(());
    };

    // `OLD` is not in this branch. PostgreSQL accepts an assignment to it and
    // runs the write anyway, measured on PostgreSQL 16, because a BEFORE
    // trigger writes what `RETURN NEW` hands back. So `OLD` is a plpgsql record
    // like any other and belongs in the message below.
    if qualifier.eq_ignore_ascii_case("new") {
        return Err(Error::UnsupportedSQLiteFeature(format!(
            "assigning to `{qualifier}.{name}` has no SQLite equivalent here, since a SQLite \
             trigger cannot change the row it fired for. It is emulated with an UPDATE over the \
             written row, which needs the whole trigger function body to be a run of assignments \
             to columns the table declares, closed by RETURN NEW."
        )));
    }

    Err(Error::UnsupportedSQLiteFeature(format!(
        "assigning to `{qualifier}.{name}` has no SQLite equivalent, since a SQLite trigger body \
         has no plpgsql record variable to hold the change. Compute the value in the statement \
         that reads it."
    )))
}

/// The `(qualifier, name)` a statement assigns to, when its target is
/// qualified.
///
/// Two rules, because the two spellings carry different evidence. `:=` only
/// ever assigns in plpgsql, so a qualified name in front of one is conclusive
/// wherever it sits. A lone `=` also compares, so it counts only where a
/// statement can start, and no SQL statement opens with a qualified identifier.
fn qualified_assignment_target(statement: &str) -> Option<(String, String)> {
    qualified_walrus_target(statement).or_else(|| qualified_equals_target(statement))
}

/// The qualified target of a `:=` assignment anywhere in `statement`.
///
/// The name is read the same way the rewrite below reads it, back from the
/// operator over identifier characters, so the two agree on what the target is.
/// A named function argument also spells `:=`, but its name is never qualified,
/// so it cannot land here.
fn qualified_walrus_target(statement: &str) -> Option<(String, String)> {
    let mut from = 0;
    while let Some(offset) = Scanner::new(&statement[from..]).find_str_in_code(":=") {
        let operator = from + offset;
        if let Some(target) = qualified_name_before(statement, operator) {
            return Some(target);
        }
        from = operator + 2;
    }
    None
}

/// The qualified target of a bare `=` assignment at a statement start.
///
/// A statement starts at the front of the chunk or just past `BEGIN`, `THEN`,
/// `ELSE`, or `LOOP`, since the caller has already split on live semicolons. A
/// `THEN` or `ELSE` inside an open `CASE` belongs to the expression rather than
/// to a branch, and skipping those is what keeps
/// `CASE WHEN c THEN t.n = 1 ELSE FALSE END` a comparison.
fn qualified_equals_target(statement: &str) -> Option<(String, String)> {
    statement_starts(statement)
        .into_iter()
        .find_map(|start| qualified_name_at_with_equals(statement, start))
}

/// What a keyword occurrence does to the scan for statement starts.
#[derive(Clone, Copy)]
enum Mark {
    /// `CASE`, whose arms are expression parts rather than statements.
    OpensCase,
    /// `END`, which closes the innermost `CASE`.
    ClosesCase,
    /// A keyword a statement can follow.
    OpensStatement,
}

/// Offsets in `statement` where a plpgsql statement can begin.
fn statement_starts(statement: &str) -> Vec<usize> {
    const KEYWORDS: [(&str, Mark); 6] = [
        ("CASE", Mark::OpensCase),
        ("END", Mark::ClosesCase),
        ("BEGIN", Mark::OpensStatement),
        ("THEN", Mark::OpensStatement),
        ("ELSE", Mark::OpensStatement),
        ("LOOP", Mark::OpensStatement),
    ];

    let scanner = Scanner::new(statement);
    let mut marks = Vec::new();
    for (keyword, mark) in KEYWORDS {
        let mut from = 0;
        while let Some(position) = scanner.find_keyword(keyword, from) {
            from = position + keyword.len();
            marks.push((position, mark, from));
        }
    }
    marks.sort_unstable_by_key(|&(position, ..)| position);

    let mut starts = vec![0];
    let mut case_depth = 0_usize;
    for (_, mark, after) in marks {
        match mark {
            Mark::OpensCase => case_depth += 1,
            // Saturating because a chunk can carry a closing `END IF` with no
            // `CASE` of its own, the caller having split at the semicolon
            // before it.
            Mark::ClosesCase => case_depth = case_depth.saturating_sub(1),
            Mark::OpensStatement if case_depth == 0 => starts.push(after),
            Mark::OpensStatement => {}
        }
    }
    starts
}

/// True for a character an unquoted identifier can carry.
///
/// One definition rather than one per reader, since a byte test and a character
/// test would disagree the moment a name is not ASCII.
fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Where the identifier that ends `text` begins, or `text.len()` when `text`
/// does not end in one.
///
/// Walks back over characters rather than searching for the last character that
/// is not an identifier one, because that search returns the offset of a
/// character and adding one to it lands inside a multi-byte character. Slicing
/// there panics, which is what a body carrying a non-ASCII name would have hit.
fn trailing_identifier_start(text: &str) -> usize {
    text.char_indices()
        .rev()
        .take_while(|&(_, character)| is_identifier_char(character))
        .last()
        .map_or(text.len(), |(index, _)| index)
}

/// Where the identifier starting at `from` ends, which is `from` when none
/// starts there.
fn identifier_end(text: &str, from: usize) -> usize {
    text[from..]
        .find(|character: char| !is_identifier_char(character))
        .map_or(text.len(), |offset| from + offset)
}

/// The first offset at or after `from` that is not ASCII whitespace.
///
/// ASCII rather than Unicode whitespace, since SQL does not treat a
/// non-breaking space as a separator and neither should this.
fn skip_ascii_whitespace(text: &str, from: usize) -> usize {
    text[from..]
        .find(|character: char| !character.is_ascii_whitespace())
        .map_or(text.len(), |offset| from + offset)
}

/// Reads `<qualifier>.<name>` followed by a lone `=` at `from`.
///
/// The `=` must stand alone, so `==` and the `=>` that spells a named function
/// argument are both rejected. A leading `<`, `>`, or `!` would already have
/// ended the name, so only what follows the `=` needs checking.
fn qualified_name_at_with_equals(statement: &str, from: usize) -> Option<(String, String)> {
    let qualifier_start = skip_ascii_whitespace(statement, from);
    let qualifier_end = identifier_end(statement, qualifier_start);
    if qualifier_end == qualifier_start || !statement[qualifier_end..].starts_with('.') {
        return None;
    }

    let name_start = qualifier_end + '.'.len_utf8();
    let name_end = identifier_end(statement, name_start);
    if name_end == name_start {
        return None;
    }

    let operator = &statement[skip_ascii_whitespace(statement, name_end)..];
    if !operator.starts_with('=') || operator.starts_with("==") || operator.starts_with("=>") {
        return None;
    }

    Some((
        statement[qualifier_start..qualifier_end].to_string(),
        statement[name_start..name_end].to_string(),
    ))
}

/// Reads `<qualifier>.<name>` backwards from `operator`.
fn qualified_name_before(statement: &str, operator: usize) -> Option<(String, String)> {
    let before = statement[..operator].trim_end();
    let (prefix, name) = before.split_at(trailing_identifier_start(before));
    if name.is_empty() {
        return None;
    }

    let qualifier_text = prefix.strip_suffix('.')?;
    let qualifier = &qualifier_text[trailing_identifier_start(qualifier_text)..];

    (!qualifier.is_empty()).then(|| (qualifier.to_string(), name.to_string()))
}

impl PlPgSqlPreprocessor {
    /// True when `body` opens an exception handler.
    ///
    /// A handler is `EXCEPTION WHEN`, which is what separates it from the
    /// `RAISE EXCEPTION` statement that shares the keyword. Read through the
    /// scanner, so neither one inside a literal or a comment counts.
    #[must_use]
    pub fn has_exception_handler(body: &str) -> bool {
        let scanner = Scanner::new(body);
        let mut from = 0;
        while let Some(position) = scanner.find_keyword("EXCEPTION", from) {
            let after = position + "EXCEPTION".len();
            if scanner
                .next_word_in_code(after)
                .is_some_and(|word| word.eq_ignore_ascii_case("when"))
            {
                return true;
            }
            from = after;
        }
        false
    }

    /// Preprocesses a PL/pgSQL function body string.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedSQLiteFeature`] when a statement assigns to
    /// a qualified target, such as `NEW.col`, outside the one shape that is
    /// translatable.
    pub fn preprocess(body: &str) -> Result<(String, PlPgSqlContext), Error> {
        let mut context = PlPgSqlContext::new();

        let (declare_section, body_section) = Self::split_declare_and_body(body);

        if let Some(declare) = declare_section {
            Self::parse_declarations(&declare, &mut context);
        }

        let transformed = Self::transform_body(&body_section, &context)?;

        Ok((transformed, context))
    }

    fn split_declare_and_body(body: &str) -> (Option<String>, String) {
        let scanner = Scanner::new(body);
        let declare_pos = scanner.find_keyword("DECLARE", 0);
        let begin_pos = scanner.find_keyword("BEGIN", 0);

        match (declare_pos, begin_pos) {
            (Some(d), Some(b)) if d < b => {
                (Some(body[d + "DECLARE".len()..b].trim().to_string()), body[b..].to_string())
            }
            (_, Some(b)) => (None, body[b..].to_string()),
            _ => (None, body.to_string()),
        }
    }

    fn parse_declarations(declare_section: &str, context: &mut PlPgSqlContext) {
        for decl in Scanner::new(declare_section).split_in_code(b';') {
            let decl = decl.trim();
            if decl.is_empty() {
                continue;
            }

            if let Some(var_decl) = Self::parse_single_declaration(decl) {
                context.add_declaration(var_decl);
            }
        }
    }

    fn parse_single_declaration(decl: &str) -> Option<VariableDeclaration> {
        let decl = decl.trim();
        if decl.is_empty() {
            return None;
        }

        let (name_type, default_value) = if let Some(pos) = decl.find(":=") {
            (decl[..pos].trim(), Some(decl[pos + 2..].trim()))
        } else if let Some(pos) = Scanner::new(decl).find_keyword("DEFAULT", 0) {
            (decl[..pos].trim(), Some(decl[pos + "DEFAULT".len()..].trim()))
        } else {
            (decl, None)
        };
        let default_value = default_value.map(single_quoted_default);

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

    fn transform_body(body: &str, context: &PlPgSqlContext) -> Result<String, Error> {
        let mut result = body.to_string();

        // Transform PostgreSQL ELSIF → ELSEIF (sqlparser uses ELSEIF keyword)
        result = Self::transform_elsif(&result);

        // Rewrite := assignments as SET statements (standalone assignments only).
        result = Self::transform_assignments(&result, context)?;

        result = Self::transform_select_into(&result, context);

        Ok(Self::transform_raise_statements(&result))
    }

    /// Rewrites RAISE statements; scans body text directly so RAISE inside
    /// IF/THEN blocks is transformed too.
    fn transform_raise_statements(body: &str) -> String {
        let mut result = String::new();
        let mut search_from = 0;
        let upper_body = body.to_uppercase();
        let scanner = Scanner::new(body);

        while let Some(raise_pos) = scanner.find_keyword("RAISE", search_from) {
            let after_raise_start = raise_pos + "RAISE ".len();
            if after_raise_start > body.len() {
                break;
            }

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
        Scanner::new(s).find_in_code(b';')
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
    ///
    /// Handled shapes, each with an optional leading `WITH` clause that moves
    /// inside the generated subquery, since `SET` has no place for it:
    ///
    /// - `SELECT <exprs> INTO <vars> FROM ...` becomes one `SET var = (SELECT
    ///   expr FROM ... LIMIT 1);` per variable.
    /// - `SELECT <exprs> INTO <vars>;` with no FROM clause, the plpgsql
    ///   spelling of `var := expr`, becomes `SET var = (SELECT expr LIMIT 1);`.
    ///
    /// Keywords are located at the top nesting level only, outside strings and
    /// parentheses, so a CTE body's own SELECT or FROM never splits the
    /// statement. Anything left untransformed keeps its INTO and is refused
    /// loudly by the trigger body translator rather than emitted.
    fn try_transform_select_into_stmt(stmt: &str, _context: &PlPgSqlContext) -> String {
        // The main SELECT is the first top-level one: CTE bodies sit inside
        // parentheses, so a WITH prefix is skipped naturally.
        let Some(select_pos) = Self::find_top_level_keyword(stmt, "SELECT", 0) else {
            return stmt.to_string();
        };
        let Some(into_pos) = Self::find_top_level_keyword(stmt, "INTO", select_pos) else {
            return stmt.to_string();
        };
        let from_pos = Self::find_top_level_keyword(stmt, "FROM", into_pos);

        // A WITH prefix belongs inside each generated subquery.
        let with_pos = Self::find_top_level_keyword(stmt, "WITH", 0).filter(|w| *w < select_pos);
        let prefix = &stmt[..with_pos.unwrap_or(select_pos)];
        let with_part = with_pos.map_or("", |w| &stmt[w..select_pos]);

        let columns_part = stmt[select_pos + "SELECT".len()..into_pos].trim();
        let vars_end = from_pos.unwrap_or_else(|| stmt.rfind(';').unwrap_or(stmt.len()));
        let vars_part = stmt[into_pos + "INTO".len()..vars_end].trim();
        let from_part = from_pos.map_or("", |f| stmt[f..].trim_end());

        let columns = Self::split_top_level_csv(columns_part);
        let vars = Self::split_top_level_csv(vars_part);

        // A variable slot that is not a bare name (INTO STRICT var, a record
        // field, ...) has no SET spelling here. Left as-is, the INTO reaches
        // the trigger body translator, which refuses it naming the statement.
        if columns.len() != vars.len()
            || columns.is_empty()
            || vars.iter().any(|var| var.chars().any(char::is_whitespace))
        {
            return stmt.to_string();
        }

        let mut result = String::new();
        result.push_str(prefix);

        // Get indentation from the position of WITH or SELECT.
        let stmt_start = with_pos.unwrap_or(select_pos);
        let line_start = stmt[..stmt_start].rfind('\n').map_or(0, |p| p + 1);
        let indent = &stmt[line_start..stmt_start];

        for (i, (col, var)) in columns.iter().zip(vars.iter()).enumerate() {
            let body = format!("{with_part}SELECT {col} {}", from_part.trim_end_matches(';'));
            // Add LIMIT 1 if not present. PostgreSQL's SELECT INTO takes the
            // first row, which a SQLite scalar subquery does anyway, so a
            // LIMIT hiding inside a CTE body suppressing this one is harmless.
            let subquery = if body.to_uppercase().contains(" LIMIT ") {
                format!("({})", body.trim_end())
            } else {
                format!("({} LIMIT 1)", body.trim_end())
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

    /// Finds the first top-level occurrence of `keyword` at or after `start`.
    ///
    /// Top-level means outside string literals and at parenthesis depth zero,
    /// and the match must be delimited by non-word characters, so `FROM` inside
    /// a CTE body or a column named `INTOX` never matches. ASCII case
    /// insensitive. Comments are not tracked, matching the rest of this
    /// preprocessor.
    fn find_top_level_keyword(stmt: &str, keyword: &str, start: usize) -> Option<usize> {
        let bytes = stmt.as_bytes();
        let mut paren_depth = 0usize;
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut chars = stmt.char_indices().peekable();

        while let Some((idx, ch)) = chars.next() {
            if in_single_quote {
                if ch == '\'' {
                    if chars.peek().is_some_and(|(_, next)| *next == '\'') {
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
                _ => {
                    if paren_depth == 0
                        && idx >= start
                        && stmt[idx..].len() >= keyword.len()
                        && stmt[idx..idx + keyword.len()].eq_ignore_ascii_case(keyword)
                        && !Self::is_word_byte(bytes.get(idx.wrapping_sub(1)).copied(), idx == 0)
                        && !Self::is_word_byte(bytes.get(idx + keyword.len()).copied(), false)
                    {
                        return Some(idx);
                    }
                }
            }
        }
        None
    }

    /// True when `byte` continues an identifier. `at_boundary` marks the string
    /// edges, which always delimit.
    fn is_word_byte(byte: Option<u8>, at_boundary: bool) -> bool {
        if at_boundary {
            return false;
        }
        byte.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
    }

    /// Transforms variable assignments from := to SET syntax.
    ///
    /// Split on unquoted semicolons rather than newlines, since the right-hand
    /// side of an assignment can span lines and a statement ends at a
    /// semicolon.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedSQLiteFeature`] when a statement assigns to
    /// a qualified target.
    fn transform_assignments(body: &str, context: &PlPgSqlContext) -> Result<String, Error> {
        let mut result = String::new();
        let mut start = 0;

        while let Some(offset) = Scanner::new(&body[start..]).find_in_code(b';') {
            let end = start + offset;
            reject_qualified_assignment(&body[start..end])?;
            result.push_str(&Self::rewrite_assignment(&body[start..end], context));
            result.push(';');
            start = end + 1;
        }
        reject_qualified_assignment(&body[start..])?;
        result.push_str(&Self::rewrite_assignment(&body[start..], context));
        Ok(result)
    }
    /// One statement, rewritten to `SET name = expr` when it is an assignment
    /// and returned unchanged when it is not.
    ///
    /// A chunk can carry text before the assignment, `BEGIN` on the first one
    /// and `END IF` after a branch, so the variable is the identifier
    /// immediately before `:=` and everything ahead of it passes through.
    fn rewrite_assignment(statement: &str, context: &PlPgSqlContext) -> String {
        let Some(operator) = Scanner::new(statement).find_str_in_code(":=") else {
            return statement.to_string();
        };
        let before = statement[..operator].trim_end();
        let (prefix, name) = before.split_at(trailing_identifier_start(before));

        let candidate = format!("{} := {}", name.trim(), statement[operator + 2..].trim());
        match Self::parse_assignment_line(&candidate, context) {
            Some(assignment) => {
                format!("{prefix}SET {} = {}", assignment.name, assignment.expression)
            }
            None => statement.to_string(),
        }
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

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn assignments_are_split_on_statements_not_lines() {
        let context = PlPgSqlContext::new();
        let body = "BEGIN\n    v_label :=\n        'a; b';\n    RETURN NEW;\nEND";
        let out = PlPgSqlPreprocessor::transform_assignments(body, &context).expect("no target");
        assert!(out.contains("SET v_label = 'a; b'"), "got: {out}");
    }

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

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");
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

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");
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

        let (transformed, _context) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");

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
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");
        assert!(
            transformed.contains("SELECT RAISE(ABORT, 'val must be non-negative')"),
            "RAISE EXCEPTION should become SELECT RAISE(ABORT, ...), got: {transformed}"
        );
        assert!(!transformed.contains("RAISE EXCEPTION"), "Original form should be removed");
    }

    #[test]
    fn test_transform_raise_exception_with_format_args() {
        let body = "BEGIN\n  RAISE EXCEPTION 'bad value: %', NEW.val;\n  RETURN NEW;\nEND";
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");
        assert!(
            transformed.contains("SELECT RAISE(ABORT, 'bad value: %')"),
            "Format args should be dropped, got: {transformed}"
        );
    }

    #[test]
    fn test_transform_raise_notice_dropped() {
        let body = "BEGIN\n  RAISE NOTICE 'debug info';\n  RETURN NEW;\nEND";
        let (transformed, _) = PlPgSqlPreprocessor::preprocess(body).expect("preprocess");
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
