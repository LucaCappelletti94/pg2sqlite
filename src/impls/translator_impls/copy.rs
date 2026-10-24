//! `COPY ... FROM STDIN`, translated into the `INSERT` that writes its inline
//! rows.
//!
//! The payload is decoded with PostgreSQL's own text and CSV rules, and each
//! field becomes a string literal, which PostgreSQL coerces to the column type
//! through the same input function `COPY` calls. The `INSERT` then takes the
//! ordinary translation, so every column conversion applies to it.

#[cfg(not(feature = "std"))]
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{iter::Peekable, str::Chars};

use sql_traits::{
    structs::ParserDB,
    traits::{ColumnLike, TableLike},
};
use sqlparser::ast::{
    ColumnOption, CopyLegacyCsvOption, CopyLegacyOption, CopyOption, CopyPayload, CopySource,
    CopyTarget, Expr, Ident, ObjectName, ObjectNamePart, Statement, TableObject, Value,
};

use super::{insert::backing_table_name, rls};
use crate::{
    errors::Error,
    impls::{ast_builder, object_name::resolve_translation_table},
    options::TranslationContext,
    traits::translator::TranslatorWithContext,
    warnings::TranslationWarning,
};

/// The parts of a `COPY` statement.
pub(super) struct Copy<'a> {
    pub(super) source: &'a CopySource,
    pub(super) to: bool,
    pub(super) target: &'a CopyTarget,
    pub(super) options: &'a [CopyOption],
    pub(super) legacy_options: &'a [CopyLegacyOption],
    pub(super) payload: Option<&'a CopyPayload>,
}

/// Translates `COPY`, which becomes an `INSERT` for an inline payload and is
/// refused in every other form.
pub(super) fn translate_copy(
    copy: &Copy<'_>,
    schema: &ParserDB,
    options: &TranslationContext<'_>,
    emit: &mut dyn FnMut(TranslationWarning),
) -> Result<Vec<Statement>, Error> {
    let &Copy { source, to, target, options: copy_options, legacy_options, payload } = copy;
    let CopySource::Table { table_name, columns } = source else {
        return Err(reject_copy("COPY (query)", to));
    };
    if to || !matches!(target, CopyTarget::Stdin) {
        return Err(reject_copy(&format!("COPY {table_name}"), to));
    }
    let Some(payload) = payload.filter(|payload| payload.terminated) else {
        return Err(Error::forward_refusal(format!(
            "COPY {table_name} FROM STDIN has no payload ended by a `\\.` line, and PostgreSQL \
             refuses a payload that runs to the end of the input. End the rows with `\\.` on a \
             line of its own."
        )));
    };
    let format = Format::new(copy_options, legacy_options)?;
    let columns =
        if columns.is_empty() { table_columns(table_name, schema)? } else { columns.clone() };
    let rows = match format.kind {
        Kind::Text => text_rows(&payload.data, &format)?,
        Kind::Csv => csv_rows(&payload.data, &format, &columns)?,
    };
    if let Some((row, fields)) =
        rows.iter().enumerate().find(|(_, fields)| fields.len() != columns.len())
    {
        return Err(Error::forward_refusal(format!(
            "row {} of COPY {table_name} has {} fields, and the statement writes {} columns, so \
             PostgreSQL refuses the whole COPY.",
            row + 1,
            fields.len(),
            columns.len()
        )));
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let values = rows
        .into_iter()
        .map(|fields| {
            fields
                .into_iter()
                .map(|field| {
                    Expr::Value(
                        field.map_or(Value::Null, Value::SingleQuotedString).with_empty_span(),
                    )
                })
                .collect()
        })
        .collect();
    let column_names = columns.into_iter().map(|column| ObjectName::from(vec![column])).collect();
    let insert =
        ast_builder::insert_body(table_name.clone(), column_names, ast_builder::values(values));
    let mut translated = insert.translate_with_warnings(schema, options, emit)?;
    // A `pg_dump` restore runs as the table owner, which PostgreSQL lets past
    // the policies, so the rows go to the backing table.
    if let Some(last) = table_name.0.last().and_then(ObjectNamePart::as_ident)
        && rls::table_has_rls(&last.value, schema)?
        && let TableObject::TableName(name) = &translated.table
    {
        translated.table = TableObject::TableName(backing_table_name(name, options));
    }
    Ok(vec![Statement::Insert(translated)])
}

/// Refuses an export or a `COPY` that reads a file or a program.
fn reject_copy(subject: &str, to: bool) -> Error {
    let advice = if to {
        "SQLite has no COPY statement, and the translator cannot write the destination. Export the \
         rows with your client instead."
    } else {
        "SQLite has no COPY statement, and the translator cannot read the source. Load the rows \
         with INSERT statements instead."
    };
    Error::forward_refusal(format!("{subject} cannot be translated. {advice}"))
}

/// The columns a `COPY` without a column list writes, every column but the
/// generated ones, in declaration order.
fn table_columns(table_name: &ObjectName, schema: &ParserDB) -> Result<Vec<Ident>, Error> {
    // PostgreSQL looks an unqualified name up in `pg_temp` first.
    let resolved = match resolve_translation_table(schema, table_name)? {
        Some(table) => Some(table),
        None => schema.resolve_table_object_name_on_search_path(table_name)?,
    };
    let Some(table) = resolved else {
        return Err(Error::forward_refusal(format!(
            "COPY {table_name} names no table the translation declares, so the columns its rows \
             fill are unknown."
        )));
    };
    Ok(table
        .columns(schema)?
        .filter(|column| {
            !column.attribute().options.iter().any(|option| {
                matches!(option.option, ColumnOption::Generated { generation_expr: Some(_), .. })
            })
        })
        .map(|column| {
            let name = column.column_name().to_string();
            if column.column_name_is_quoted() {
                Ident::with_quote('"', name)
            } else {
                Ident::new(name)
            }
        })
        .collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Csv,
}

/// How a payload is decoded, after PostgreSQL's defaults and checks.
struct Format {
    kind: Kind,
    delimiter: char,
    null: String,
    header: bool,
    quote: char,
    escape: char,
    force_null: Vec<Ident>,
    force_not_null: Vec<Ident>,
}

/// The options as written, each at most once.
#[derive(Default)]
struct Written {
    kind: Option<Kind>,
    delimiter: Option<char>,
    null: Option<String>,
    header: Option<bool>,
    quote: Option<char>,
    escape: Option<char>,
    force_null: Option<Vec<Ident>>,
    force_not_null: Option<Vec<Ident>>,
}

impl Format {
    fn new(options: &[CopyOption], legacy: &[CopyLegacyOption]) -> Result<Self, Error> {
        let mut written = Written::default();
        for option in options {
            match option {
                CopyOption::Format(name) => {
                    let kind = match name.value.to_ascii_lowercase().as_str() {
                        "text" => Kind::Text,
                        "csv" => Kind::Csv,
                        _ => return Err(unsupported_option(&option.to_string())),
                    };
                    once(&mut written.kind, kind, "FORMAT")?;
                }
                CopyOption::Delimiter(delimiter) => {
                    once(&mut written.delimiter, *delimiter, "DELIMITER")?;
                }
                CopyOption::Null(null) => once(&mut written.null, null.clone(), "NULL")?,
                CopyOption::Header(header) => once(&mut written.header, *header, "HEADER")?,
                CopyOption::Quote(quote) => once(&mut written.quote, *quote, "QUOTE")?,
                CopyOption::Escape(escape) => once(&mut written.escape, *escape, "ESCAPE")?,
                CopyOption::ForceNull(columns) => {
                    once(&mut written.force_null, columns.clone(), "FORCE_NULL")?;
                }
                CopyOption::ForceNotNull(columns) => {
                    once(&mut written.force_not_null, columns.clone(), "FORCE_NOT_NULL")?;
                }
                CopyOption::Freeze(_) | CopyOption::ForceQuote(_) | CopyOption::Encoding(_) => {
                    return Err(unsupported_option(&option.to_string()));
                }
            }
        }
        for option in legacy {
            match option {
                CopyLegacyOption::Delimiter(delimiter) => {
                    once(&mut written.delimiter, *delimiter, "DELIMITER")?;
                }
                CopyLegacyOption::Null(null) => once(&mut written.null, null.clone(), "NULL")?,
                CopyLegacyOption::Csv(csv) => {
                    once(&mut written.kind, Kind::Csv, "CSV")?;
                    for csv_option in csv {
                        match csv_option {
                            CopyLegacyCsvOption::Header => {
                                once(&mut written.header, true, "HEADER")?;
                            }
                            CopyLegacyCsvOption::Quote(quote) => {
                                once(&mut written.quote, *quote, "QUOTE")?;
                            }
                            CopyLegacyCsvOption::Escape(escape) => {
                                once(&mut written.escape, *escape, "ESCAPE")?;
                            }
                            CopyLegacyCsvOption::ForceNotNull(columns) => {
                                once(
                                    &mut written.force_not_null,
                                    columns.clone(),
                                    "FORCE NOT NULL",
                                )?;
                            }
                            CopyLegacyCsvOption::ForceQuote(_) => {
                                return Err(unsupported_option(&csv_option.to_string()));
                            }
                        }
                    }
                }
                other => return Err(unsupported_option(&other.to_string())),
            }
        }
        Self::checked(written)
    }

    /// Applies PostgreSQL's defaults and refuses what its option check refuses.
    fn checked(written: Written) -> Result<Self, Error> {
        let kind = written.kind.unwrap_or(Kind::Text);
        let csv = kind == Kind::Csv;
        if !csv {
            for (name, given) in [
                ("QUOTE", written.quote.is_some()),
                ("ESCAPE", written.escape.is_some()),
                ("FORCE_NULL", written.force_null.is_some()),
                ("FORCE_NOT_NULL", written.force_not_null.is_some()),
            ] {
                if given {
                    return Err(invalid_option(&format!("COPY {name} requires CSV mode")));
                }
            }
        }
        let delimiter = written.delimiter.unwrap_or(if csv { ',' } else { '\t' });
        let quote = written.quote.unwrap_or('"');
        let escape = written.escape.unwrap_or(quote);
        let null = written.null.unwrap_or_else(|| if csv { String::new() } else { "\\N".into() });
        if matches!(delimiter, '\n' | '\r') {
            return Err(invalid_option("COPY delimiter cannot be newline or carriage return"));
        }
        if null.contains(['\n', '\r']) {
            return Err(invalid_option(
                "COPY null representation cannot use newline or carriage return",
            ));
        }
        if !csv && "\\.abcdefghijklmnopqrstuvwxyz0123456789".contains(delimiter) {
            return Err(invalid_option(&format!("COPY delimiter cannot be \"{delimiter}\"")));
        }
        if csv && delimiter == quote {
            return Err(invalid_option("COPY delimiter and quote must be different"));
        }
        if null.contains(delimiter) {
            return Err(invalid_option(
                "COPY delimiter character must not appear in the NULL specification",
            ));
        }
        if csv && null.contains(quote) {
            return Err(invalid_option(
                "CSV quote character must not appear in the NULL specification",
            ));
        }
        Ok(Self {
            kind,
            delimiter,
            null,
            header: written.header.unwrap_or(false),
            quote,
            escape,
            force_null: written.force_null.unwrap_or_default(),
            force_not_null: written.force_not_null.unwrap_or_default(),
        })
    }
}

fn once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), Error> {
    if slot.replace(value).is_some() {
        return Err(invalid_option(&format!("COPY {name} is given more than once")));
    }
    Ok(())
}

fn unsupported_option(option: &str) -> Error {
    Error::forward_refusal(format!(
        "COPY option {option} cannot be translated. Only the text and CSV formats with DELIMITER, \
         NULL, HEADER, QUOTE, ESCAPE, FORCE_NULL and FORCE_NOT_NULL become INSERT statements."
    ))
}

fn invalid_option(reason: &str) -> Error {
    Error::forward_refusal(format!("{reason}, so PostgreSQL refuses the COPY."))
}

fn malformed(record: usize, reason: &str) -> Error {
    Error::forward_refusal(format!(
        "row {record} of the COPY payload {reason}, so PostgreSQL refuses the whole COPY."
    ))
}

/// The line ending a payload uses, which PostgreSQL requires to be the same
/// on every line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineEnd {
    Lf,
    CrLf,
    Cr,
}

/// Consumes the line ending that starts with `first`, and refuses one that
/// differs from the payload's first.
fn line_end(
    first: char,
    chars: &mut Peekable<Chars<'_>>,
    seen: &mut Option<LineEnd>,
    record: usize,
) -> Result<(), Error> {
    let end = match first {
        '\r' if chars.peek() == Some(&'\n') => {
            chars.next();
            LineEnd::CrLf
        }
        '\r' => LineEnd::Cr,
        _ => LineEnd::Lf,
    };
    if *seen.get_or_insert(end) == end {
        Ok(())
    } else {
        Err(malformed(record, "ends with a different line ending than the first row"))
    }
}

/// Decodes the text format, matching the null string before any backslash
/// escape is resolved.
fn text_rows(data: &str, format: &Format) -> Result<Vec<Vec<Option<String>>>, Error> {
    let mut rows = Vec::new();
    let mut chars = data.chars().peekable();
    let mut seen = None;
    let mut record = 0;
    while chars.peek().is_some() {
        record += 1;
        let mut fields = Vec::new();
        let mut raw = String::new();
        let mut value = Vec::new();
        loop {
            match chars.next() {
                None => break,
                Some(end @ ('\n' | '\r')) => {
                    line_end(end, &mut chars, &mut seen, record)?;
                    break;
                }
                Some(character) if character == format.delimiter => {
                    let field = text_field(&raw, value, format, record)?;
                    fields.push(field);
                    raw.clear();
                    value = Vec::new();
                }
                Some('\\') => {
                    raw.push('\\');
                    let Some(escaped) = chars.next() else { break };
                    raw.push(escaped);
                    unescape(escaped, &mut chars, &mut raw, &mut value);
                }
                Some(character) => {
                    raw.push(character);
                    push_char(&mut value, character);
                }
            }
        }
        fields.push(text_field(&raw, value, format, record)?);
        if !(format.header && record == 1) {
            rows.push(fields);
        }
    }
    Ok(rows)
}

/// Resolves the backslash escape whose character is `escaped`.
fn unescape(escaped: char, chars: &mut Peekable<Chars<'_>>, raw: &mut String, value: &mut Vec<u8>) {
    let byte = match escaped {
        'b' => 0x08,
        'f' => 0x0c,
        'n' => b'\n',
        'r' => b'\r',
        't' => b'\t',
        'v' => 0x0b,
        '0'..='7' => {
            let mut code = escaped.to_digit(8).unwrap_or_default();
            for _ in 0..2 {
                let Some(digit) = chars.peek().and_then(|next| next.to_digit(8)) else { break };
                raw.extend(chars.next());
                code = code * 8 + digit;
            }
            // PostgreSQL keeps the low byte of a three-digit octal escape.
            u8::try_from(code & 0o377).unwrap_or_default()
        }
        'x' if chars.peek().is_some_and(char::is_ascii_hexdigit) => {
            let mut code = 0;
            for _ in 0..2 {
                let Some(digit) = chars.peek().and_then(|next| next.to_digit(16)) else { break };
                raw.extend(chars.next());
                code = code * 16 + digit;
            }
            u8::try_from(code).unwrap_or_default()
        }
        other => {
            push_char(value, other);
            return;
        }
    };
    value.push(byte);
}

fn push_char(value: &mut Vec<u8>, character: char) {
    value.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
}

fn text_field(
    raw: &str,
    value: Vec<u8>,
    format: &Format,
    record: usize,
) -> Result<Option<String>, Error> {
    if raw == format.null {
        return Ok(None);
    }
    String::from_utf8(value)
        .map(Some)
        .map_err(|_| malformed(record, "decodes to bytes that are not valid UTF-8"))
}

/// Decodes the CSV format, where only an unquoted field matches the null
/// string unless `FORCE_NULL` or `FORCE_NOT_NULL` says otherwise.
fn csv_rows(
    data: &str,
    format: &Format,
    columns: &[Ident],
) -> Result<Vec<Vec<Option<String>>>, Error> {
    let force_null = positions(&format.force_null, columns, "FORCE_NULL")?;
    let force_not_null = positions(&format.force_not_null, columns, "FORCE_NOT_NULL")?;
    let field = |value: String, quoted: bool, position: usize| {
        let null = value == format.null
            && if quoted {
                force_null.contains(&position)
            } else {
                !force_not_null.contains(&position)
            };
        (!null).then_some(value)
    };
    let mut rows = Vec::new();
    let mut chars = data.chars().peekable();
    let mut seen = None;
    let mut record = 0;
    while chars.peek().is_some() {
        record += 1;
        let mut fields = Vec::new();
        let mut value = String::new();
        let (mut quoted, mut in_quotes) = (false, false);
        loop {
            let next = chars.next();
            if in_quotes {
                match next {
                    None => return Err(malformed(record, "ends inside a quoted field")),
                    Some(character)
                        if character == format.escape
                            && chars.peek().is_some_and(|&after| {
                                after == format.quote || after == format.escape
                            }) =>
                    {
                        value.extend(chars.next());
                    }
                    Some(character) if character == format.quote => in_quotes = false,
                    Some(character) => value.push(character),
                }
                continue;
            }
            match next {
                None => break,
                Some(end @ ('\n' | '\r')) => {
                    line_end(end, &mut chars, &mut seen, record)?;
                    break;
                }
                Some(character) if character == format.delimiter => {
                    fields.push(field(core::mem::take(&mut value), quoted, fields.len()));
                    quoted = false;
                }
                Some(character) if character == format.quote => (quoted, in_quotes) = (true, true),
                Some(character) => value.push(character),
            }
        }
        fields.push(field(value, quoted, fields.len()));
        if !(format.header && record == 1) {
            rows.push(fields);
        }
    }
    Ok(rows)
}

/// The positions of `named` among the columns the `COPY` writes.
fn positions(named: &[Ident], columns: &[Ident], option: &str) -> Result<Vec<usize>, Error> {
    named
        .iter()
        .map(|name| {
            columns.iter().position(|column| same_column(column, name)).ok_or_else(|| {
                invalid_option(&format!("{option} column \"{name}\" is not referenced by COPY"))
            })
        })
        .collect()
}

fn same_column(left: &Ident, right: &Ident) -> bool {
    if left.quote_style.is_some() && right.quote_style.is_some() {
        left.value == right.value
    } else {
        left.value.eq_ignore_ascii_case(&right.value)
    }
}
