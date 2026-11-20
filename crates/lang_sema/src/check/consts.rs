//! Constants: `const LIMIT = 4 * 1024` (docs/11, milestone 1.4).
//!
//! A constant's value is checked like any expression, then folded to a literal with exactly the
//! semantics the generated code would give the same operators at run time, so the rest of the
//! compiler still sees only literals. A constant may use other constants, in the same module or
//! imported, whatever their order in the file: each is checked the first time something needs it,
//! and one that needs itself is reported with the chain of constants that led back to it.

use super::{Checker, FnCx, error_expr};
use crate::hir::{self, BinaryOp, ExprKind, UnaryOp};
use crate::types::Ty;
use lang_diag::{Diagnostic, Span, codes};

/// Where a constant is in being checked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ConstState {
    Pending,
    /// Being checked now: meeting it again means it is defined in terms of itself.
    Checking,
    Done,
}

/// The longest string a constant may build with `*`, so that a typo cannot make the compiler
/// allocate gigabytes.
const MAX_CONST_STRING: usize = 1 << 20;

/// Why an expression is not a constant: where, and what to tell the user. `None` when the
/// expression already had an error that was reported.
struct NotConst(Option<(Span, String, Option<&'static str>)>);

impl Checker<'_, '_> {
    pub(super) fn check_consts(&mut self) {
        for (index, _) in self.const_items.clone() {
            self.check_const(index);
        }
    }

    /// Check constant `index` unless that has been done, or is being done.
    pub(super) fn check_const(&mut self, index: usize) {
        if self.consts[index].state != ConstState::Pending {
            return;
        }
        let Some(&(_, decl)) = self.const_items.iter().find(|(i, _)| *i == index) else { return };
        self.consts[index].state = ConstState::Checking;
        self.const_stack.push(index);
        let module = self.module_of(decl.name.span);
        let value = self.in_module(module, |this| {
            let mut cx = FnCx::detached();
            let value = match &decl.ty {
                Some(hint) => {
                    let ty = this.resolve_value_type(hint);
                    let reason = Some((hint.span, "expected because of this type hint".to_string()));
                    this.expect(&mut cx, &decl.value, ty, reason)
                }
                None => this.expr(&mut cx, &decl.value, None),
            };
            match fold(&value) {
                Ok(literal) => Some(literal),
                Err(NotConst(None)) => None,
                Err(NotConst(Some((span, label, help)))) => {
                    let mut diagnostic = Diagnostic::error(
                        codes::CONST_NOT_LITERAL,
                        format!("the value of constant `{}` cannot be worked out when compiling", decl.name.name),
                        span,
                        label,
                    );
                    diagnostic = diagnostic.with_help(help.unwrap_or(
                        "use numbers, strings, `True` and `False`, other constants and operators; calls and collections are not allowed",
                    ));
                    this.error(diagnostic);
                    None
                }
            }
        });
        self.const_stack.pop();
        self.consts[index].value = value;
        self.consts[index].state = ConstState::Done;
    }

    /// The value of constant `index` where it is used at `span`.
    pub(super) fn const_value(&mut self, index: usize, name: &str, span: Span) -> hir::Expr {
        match self.consts[index].state {
            ConstState::Pending => self.check_const(index),
            ConstState::Checking => {
                // Name the constants from the first use of this one, around back to it.
                let start = self.const_stack.iter().position(|&i| i == index).unwrap_or(0);
                let mut chain: Vec<String> = self.const_stack[start..]
                    .iter()
                    .filter_map(|&i| self.const_items.iter().find(|(c, _)| *c == i))
                    .map(|(_, decl)| format!("`{}`", decl.name.name))
                    .collect();
                chain.push(format!("`{name}`"));
                self.error(
                    Diagnostic::error(
                        codes::CONST_NOT_LITERAL,
                        format!("constant `{name}` is defined in terms of itself"),
                        span,
                        "used again here",
                    )
                    .with_help(format!("the chain is {}", chain.join(" -> "))),
                );
                return error_expr(span);
            }
            ConstState::Done => {}
        }
        match &self.consts[index].value {
            Some(value) => hir::Expr { span, ..value.clone() },
            None => error_expr(span),
        }
    }
}

fn literal(kind: ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

fn refuse<T>(span: Span, label: &str, help: Option<&'static str>) -> Result<T, NotConst> {
    Err(NotConst(Some((span, label.to_string(), help))))
}

/// Evaluate a checked expression made of literals and operators, as the generated code would.
fn fold(expr: &hir::Expr) -> Result<hir::Expr, NotConst> {
    if expr.ty == Ty::ERROR {
        return Err(NotConst(None));
    }
    let span = expr.span;
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => Ok(expr.clone()),
        ExprKind::Unary { op, operand } => {
            let operand = fold(operand)?;
            let kind = match (op, &operand.kind) {
                (UnaryOp::IntNeg, ExprKind::Int(v)) => ExprKind::Int(v.wrapping_neg()),
                (UnaryOp::IntInvert, ExprKind::Int(v)) => ExprKind::Int(!v),
                (UnaryOp::FloatNeg, ExprKind::Float(v)) => ExprKind::Float(-v),
                (UnaryOp::Not, ExprKind::Bool(v)) => ExprKind::Bool(!v),
                _ => return refuse(span, "not a constant", None),
            };
            Ok(literal(kind, expr.ty, span))
        }
        ExprKind::And(lhs, rhs) | ExprKind::Or(lhs, rhs) => {
            let is_and = matches!(expr.kind, ExprKind::And(..));
            let (ExprKind::Bool(l), ExprKind::Bool(r)) = (fold(lhs)?.kind, fold(rhs)?.kind) else {
                return refuse(span, "not a constant", None);
            };
            Ok(literal(ExprKind::Bool(if is_and { l && r } else { l || r }), expr.ty, span))
        }
        ExprKind::IfElse { cond, then, otherwise } => {
            let ExprKind::Bool(cond) = fold(cond)?.kind else { return refuse(span, "not a constant", None) };
            // Both branches must be constants, as the one not taken still has to compile.
            let (then, otherwise) = (fold(then)?, fold(otherwise)?);
            let mut chosen = if cond { then } else { otherwise };
            chosen.span = span;
            Ok(chosen)
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let (l, r) = (fold(lhs)?, fold(rhs)?);
            let kind = match (&l.kind, &r.kind) {
                (ExprKind::Int(a), ExprKind::Int(b)) => fold_int(*op, *a, *b, span)?,
                (ExprKind::Float(a), ExprKind::Float(b)) => fold_float(*op, *a, *b, span)?,
                (ExprKind::Bool(a), ExprKind::Bool(b)) => match op {
                    BinaryOp::Eq => ExprKind::Bool(a == b),
                    BinaryOp::NotEq => ExprKind::Bool(a != b),
                    _ => return refuse(span, "not a constant", None),
                },
                (ExprKind::Str(a), ExprKind::Str(b)) => fold_str(*op, a, b, span)?,
                (ExprKind::Str(a), ExprKind::Int(n)) if *op == BinaryOp::StrRepeat => {
                    let times = usize::try_from(*n).unwrap_or(0);
                    if a.len().saturating_mul(times) > MAX_CONST_STRING {
                        return refuse(span, "too long", Some("a constant string can be at most 1 MiB"));
                    }
                    ExprKind::Str(a.repeat(times))
                }
                _ => return refuse(span, "not a constant", None),
            };
            Ok(literal(kind, expr.ty, span))
        }
        ExprKind::Call { .. } | ExprKind::Builtin { .. } => {
            refuse(span, "a call", Some("constants are worked out when compiling, so they cannot call functions"))
        }
        ExprKind::List(_) | ExprKind::Dict(_) | ExprKind::Tuple(_) => refuse(
            span,
            "a collection",
            Some("a constant is a number, string or `True`/`False`; build a collection in a function"),
        ),
        ExprKind::FString(_) => {
            refuse(span, "an f-string", Some("join strings with `+`, or build the text in a function"))
        }
        _ => refuse(span, "not a constant", None),
    }
}

fn fold_int(op: BinaryOp, a: i64, b: i64, span: Span) -> Result<ExprKind, NotConst> {
    use BinaryOp::*;
    Ok(match op {
        IntAdd => ExprKind::Int(a.wrapping_add(b)),
        IntSub => ExprKind::Int(a.wrapping_sub(b)),
        IntMul => ExprKind::Int(a.wrapping_mul(b)),
        IntAnd => ExprKind::Int(a & b),
        IntOr => ExprKind::Int(a | b),
        IntXor => ExprKind::Int(a ^ b),
        IntFloorDiv | IntMod | IntTrueDiv if b == 0 => {
            return refuse(span, "division by zero", Some("dividing by zero is an error here as it is when running"));
        }
        IntFloorDiv | IntMod => {
            // `MIN // -1` wraps, like other overflow, and the remainder is then 0.
            let (quotient, remainder) = if b == -1 { (a.wrapping_neg(), 0) } else { (a / b, a % b) };
            let adjust = remainder != 0 && (remainder < 0) != (b < 0);
            if op == IntFloorDiv {
                ExprKind::Int(quotient - i64::from(adjust))
            } else {
                ExprKind::Int(if adjust { remainder + b } else { remainder })
            }
        }
        IntTrueDiv => ExprKind::Float(a as f64 / b as f64),
        IntPow => {
            if b < 0 {
                return refuse(
                    span,
                    "negative exponent",
                    Some("an `int` cannot be raised to a negative power; convert to `float` first"),
                );
            }
            let (mut result, mut base, mut exp) = (1i64, a, b as u64);
            while exp > 0 {
                if exp & 1 == 1 {
                    result = result.wrapping_mul(base);
                }
                base = base.wrapping_mul(base);
                exp >>= 1;
            }
            ExprKind::Int(result)
        }
        IntShl | IntShr => {
            if b < 0 {
                return refuse(span, "negative shift count", Some("a shift count cannot be negative"));
            }
            let count = b.min(63) as u32;
            if op == IntShl { ExprKind::Int(if b > 63 { 0 } else { a << count }) } else { ExprKind::Int(a >> count) }
        }
        Eq => ExprKind::Bool(a == b),
        NotEq => ExprKind::Bool(a != b),
        Lt => ExprKind::Bool(a < b),
        Le => ExprKind::Bool(a <= b),
        Gt => ExprKind::Bool(a > b),
        Ge => ExprKind::Bool(a >= b),
        _ => return refuse(span, "not a constant", None),
    })
}

fn fold_float(op: BinaryOp, a: f64, b: f64, span: Span) -> Result<ExprKind, NotConst> {
    use BinaryOp::*;
    Ok(match op {
        FloatAdd => ExprKind::Float(a + b),
        FloatSub => ExprKind::Float(a - b),
        FloatMul => ExprKind::Float(a * b),
        FloatDiv => ExprKind::Float(a / b),
        FloatFloorDiv => ExprKind::Float((a / b).floor()),
        FloatMod => {
            // Python's `%` takes the sign of the divisor.
            let rem = a % b;
            ExprKind::Float(if rem != 0.0 && (rem < 0.0) != (b < 0.0) { rem + b } else { rem })
        }
        FloatPow => ExprKind::Float(a.powf(b)),
        Eq => ExprKind::Bool(a == b),
        NotEq => ExprKind::Bool(a != b),
        Lt => ExprKind::Bool(a < b),
        Le => ExprKind::Bool(a <= b),
        Gt => ExprKind::Bool(a > b),
        Ge => ExprKind::Bool(a >= b),
        _ => return refuse(span, "not a constant", None),
    })
}

fn fold_str(op: BinaryOp, a: &str, b: &str, span: Span) -> Result<ExprKind, NotConst> {
    use BinaryOp::*;
    Ok(match op {
        StrConcat => {
            if a.len() + b.len() > MAX_CONST_STRING {
                return refuse(span, "too long", Some("a constant string can be at most 1 MiB"));
            }
            ExprKind::Str(format!("{a}{b}"))
        }
        Eq => ExprKind::Bool(a == b),
        NotEq => ExprKind::Bool(a != b),
        Lt => ExprKind::Bool(a < b),
        Le => ExprKind::Bool(a <= b),
        Gt => ExprKind::Bool(a > b),
        Ge => ExprKind::Bool(a >= b),
        _ => return refuse(span, "not a constant", None),
    })
}
