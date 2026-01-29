//! Helpers for generating UUIDs in Pure SQL (SQLite compatible).
//!
//! This module contains helper functions that construct `sqlparser::ast::Expr`
//! nodes representing complex SQLite expressions for generating UUIDs (v4 and
//! v7).

use sqlparser::ast::{
    BinaryOperator, CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, Ident, ObjectName, ObjectNamePart, Value,
};

/// Helper functions for building SQL expressions.
///
/// # Arguments
///
/// * `name` - The name of the function to create.
/// * `args` - A vector of expressions representing the arguments to the
///   function.
fn func(name: &str, args: Vec<Expr>) -> Expr {
    let args = args.into_iter().map(|e| FunctionArg::Unnamed(FunctionArgExpr::Expr(e))).collect();

    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(name))]),
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args,
            clauses: vec![],
        }),
        parameters: FunctionArguments::None,
        filter: None,
        null_treatment: None,
        over: None,
        within_group: vec![],
        uses_odbc_syntax: false,
    })
}

fn box_expr(e: Expr) -> Box<Expr> {
    Box::new(e)
}

fn nested(e: Expr) -> Expr {
    Expr::Nested(box_expr(e))
}

fn concat(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp {
        left: box_expr(left),
        op: BinaryOperator::StringConcat,
        right: box_expr(right),
    }
}

fn bit_and(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: box_expr(left), op: BinaryOperator::BitwiseAnd, right: box_expr(right) }
}

fn bit_or(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: box_expr(left), op: BinaryOperator::BitwiseOr, right: box_expr(right) }
}

fn mul(left: Expr, right: Expr) -> Expr {
    Expr::BinaryOp { left: box_expr(left), op: BinaryOperator::Multiply, right: box_expr(right) }
}

fn num(n: i64) -> Expr {
    Expr::Value(Value::Number(n.to_string(), false).into())
}

fn text(s: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(s.to_string()).into())
}

// Helpers for specific functions
fn random() -> Expr {
    func("random", vec![])
}
fn abs(e: Expr) -> Expr {
    func("abs", vec![e])
}
fn printf(fmt: &str, args: Vec<Expr>) -> Expr {
    let mut all_args = vec![text(fmt)];
    all_args.extend(args);
    func("printf", all_args)
}
fn hex(e: Expr) -> Expr {
    func("hex", vec![e])
}
fn unhex(e: Expr) -> Expr {
    func("unhex", vec![e])
}
fn lower(e: Expr) -> Expr {
    func("lower", vec![e])
}
fn randomblob(size: i64) -> Expr {
    func("randomblob", vec![num(size)])
}
fn substr(str_expr: Expr, start: i64, len: i64) -> Expr {
    func("substr", vec![str_expr, num(start), num(len)])
}
fn unix_ms() -> Expr {
    // CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
    let julian_now = func("julianday", vec![text("now")]);
    let epoch_jd = Expr::Value(Value::Number("2440587.5".to_string(), false).into());

    let diff = Expr::BinaryOp {
        left: box_expr(julian_now),
        op: BinaryOperator::Minus,
        right: box_expr(epoch_jd),
    };

    let ms_per_day = Expr::Value(Value::Number("86400000.0".to_string(), false).into());

    Expr::Cast {
        expr: box_expr(mul(nested(diff), ms_per_day)),
        data_type: DataType::Integer(None),
        format: None,
        kind: CastKind::Cast,
    }
}

#[must_use]
/// Generates a V4 UUID as a TEXT string.
///
/// Returns an expression equivalent to:
/// `lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-' ||
/// printf('%04x', (abs(random()) & 4095) | 16384) || '-' || printf('%04x',
/// (abs(random()) & 16383) | 32768) || '-' || hex(randomblob(6)))`
pub fn generate_uuid_v4_text() -> Expr {
    let part1 = hex(randomblob(4));
    let hyphen = text("-");
    let part2 = hex(randomblob(2));

    // (abs(random()) & 4095) | 16384
    let ver_expr = bit_or(nested(bit_and(abs(random()), num(4095))), num(16384));
    let part3 = printf("%04x", vec![ver_expr]);

    // (abs(random()) & 16383) | 32768
    let var_expr2 = bit_or(nested(bit_and(abs(random()), num(16383))), num(32768));
    let part4 = printf("%04x", vec![var_expr2]);

    let part5 = hex(randomblob(6));

    let joined = concat(
        part1,
        concat(
            hyphen.clone(),
            concat(
                part2,
                concat(
                    hyphen.clone(),
                    concat(part3, concat(hyphen.clone(), concat(part4, concat(hyphen, part5)))),
                ),
            ),
        ),
    );

    lower(joined)
}

#[must_use]
/// Generates a V4 UUID as a BLOB.
///
/// Returns an expression equivalent to:
/// `randomblob(6) || unhex(printf('%02x', (random() & 15) | 64)) ||
/// unhex(printf('%02x', random() & 255)) || unhex(printf('%02x', (random() &
/// 63) | 128)) || randomblob(7)`
pub fn generate_uuid_v4_blob() -> Expr {
    let part1 = randomblob(6);

    // (random() & 15) | 64
    let ver_val = bit_or(nested(bit_and(random(), num(15))), num(64));
    let part2 = unhex(printf("%02x", vec![ver_val]));

    // random() & 255
    let rand_val = bit_and(random(), num(255));
    let part3 = unhex(printf("%02x", vec![rand_val]));

    // (random() & 63) | 128
    let var_val2 = bit_or(nested(bit_and(random(), num(63))), num(128));
    let part4 = unhex(printf("%02x", vec![var_val2]));

    let part5 = randomblob(7);

    concat(part1, concat(part2, concat(part3, concat(part4, part5))))
}

#[must_use]
/// Generates a V7 UUID as a TEXT string.
///
/// Uses `julianday('now')` for the timestamp component to ensure millisecond
/// precision.
pub fn generate_uuid_v7_text() -> Expr {
    let ts_hex = printf("%012x", vec![unix_ms()]);

    let part1 = substr(ts_hex.clone(), 1, 8);
    let hyphen = text("-");
    let part2 = substr(ts_hex, 9, 4);

    // (abs(random()) & 4095) | 28672  -> 0x7000 | ...
    let ver_expr = bit_or(nested(bit_and(abs(random()), num(4095))), num(28672));
    let part3 = printf("%04x", vec![ver_expr]);

    // (abs(random()) & 16383) | 32768 -> 0x8000 | ...
    let var_expr2 = bit_or(nested(bit_and(abs(random()), num(16383))), num(32768));
    let part4 = printf("%04x", vec![var_expr2]);

    // printf('%04x%04x%04x', abs(random()) & 65535, abs(random()) & 65535,
    // abs(random()) & 65535)
    let part5 = printf(
        "%04x%04x%04x",
        vec![
            bit_and(abs(random()), num(65535)),
            bit_and(abs(random()), num(65535)),
            bit_and(abs(random()), num(65535)),
        ],
    );

    let joined = concat(
        part1,
        concat(
            hyphen.clone(),
            concat(
                part2,
                concat(
                    hyphen.clone(),
                    concat(part3, concat(hyphen.clone(), concat(part4, concat(hyphen, part5)))),
                ),
            ),
        ),
    );

    lower(joined)
}

#[must_use]
/// Generates a V7 UUID as a BLOB.
///
/// Uses `julianday('now')` for the timestamp component.
pub fn generate_uuid_v7_blob() -> Expr {
    let ts_hex = printf("%012x", vec![unix_ms()]);

    let part1 = unhex(substr(ts_hex, 1, 12));

    // (random() & 15) | 112  -> 0x70 | ...
    let ver_val = bit_or(nested(bit_and(random(), num(15))), num(112));
    let part2 = unhex(printf("%02x", vec![ver_val]));

    // random() & 255
    let rand_val = bit_and(random(), num(255));
    let part3 = unhex(printf("%02x", vec![rand_val]));

    // (random() & 63) | 128
    let var_val2 = bit_or(nested(bit_and(random(), num(63))), num(128));
    let part4 = unhex(printf("%02x", vec![var_val2]));

    let part5 = randomblob(7);

    concat(part1, concat(part2, concat(part3, concat(part4, part5))))
}
