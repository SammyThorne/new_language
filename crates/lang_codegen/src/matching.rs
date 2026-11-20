//! `match`: each case's pattern becomes a chain of tests that jump to the next case on the
//! first failure, binding names as it goes; then the guard, then the body. The type checker has
//! proved the cases exhaustive, so falling off the last case is unreachable.
//!
//! Cases are tested in order. Sharing tests between cases (a decision tree) would make large
//! matches faster and is left as an optimisation.

use crate::{Codegen, Must};
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, IntValue};
use lang_sema::hir::{Expr, MatchCase, Pattern, PatternKind, Place};

impl<'ctx> Codegen<'ctx, '_> {
    pub(crate) fn match_stmt(&mut self, subject: &Expr, cases: &[MatchCase]) {
        let value = self.expr(subject);
        let end = self.block("endmatch");
        for case in cases {
            let next = self.block("nextcase");
            self.pattern(&case.pattern, value, next);
            if let Some(guard) = &case.guard {
                let cond = self.value(guard).into_int_value();
                let body = self.block("case");
                self.b.build_conditional_branch(cond, body, next).must();
                self.b.position_at_end(body);
            }
            self.stmts(&case.body);
            self.b.build_unconditional_branch(end).must();
            self.b.position_at_end(next);
        }
        self.b.build_unreachable().must();
        self.b.position_at_end(end);
    }

    fn require(&mut self, matched: IntValue<'ctx>, fail: BasicBlock<'ctx>) {
        let ok = self.block("matched");
        self.b.build_conditional_branch(matched, ok, fail).must();
        self.b.position_at_end(ok);
    }

    /// Test `value` against `pattern`, jumping to `fail` if it does not match.
    fn pattern(&mut self, pattern: &Pattern, value: Option<BasicValueEnum<'ctx>>, fail: BasicBlock<'ctx>) {
        match &pattern.kind {
            PatternKind::Wild => {}
            PatternKind::Bind(local) => self.store(&Place::Local(*local), value),
            PatternKind::Literal(expr) => {
                let literal = self.value(expr);
                let equal = self.emit_eq(pattern.ty, value.unwrap(), literal);
                self.require(equal, fail);
            }
            PatternKind::Variant { index, fields } => {
                let value = value.unwrap();
                let tag = self.enum_tag(pattern.ty, value);
                let expected = self.i64(*index as i64);
                let matched = self.b.build_int_compare(IntPredicate::EQ, tag, expected, "").must();
                self.require(matched, fail);
                for (i, field) in fields.iter().enumerate() {
                    if matches!(field.kind, PatternKind::Wild) {
                        continue;
                    }
                    let field_value = self.variant_field(pattern.ty, *index, i, value);
                    self.pattern(field, Some(field_value), fail);
                }
            }
            PatternKind::Some(inner) => {
                let value = value.unwrap();
                let present = self.opt_is_some(pattern.ty, value);
                self.require(present, fail);
                let inner_value = self.opt_inner(pattern.ty, value);
                self.pattern(inner, Some(inner_value), fail);
            }
            PatternKind::None => {
                let present = self.opt_is_some(pattern.ty, value.unwrap());
                let absent = self.b.build_not(present, "").must();
                self.require(absent, fail);
            }
            PatternKind::Ok(inner) => {
                let value = value.unwrap();
                let ok = self.result_is_ok(value);
                self.require(ok, fail);
                let payload = self.result_ok(pattern.ty, value);
                self.pattern(inner, payload, fail);
            }
            PatternKind::Err(inner) => {
                let value = value.unwrap();
                let ok = self.result_is_ok(value);
                let failed = self.b.build_not(ok, "").must();
                self.require(failed, fail);
                let error = self.result_err(pattern.ty, value);
                self.pattern(inner, Some(error), fail);
            }
            PatternKind::Tuple(items) => {
                let tuple = value.unwrap().into_struct_value();
                for (index, item) in items.iter().enumerate() {
                    if matches!(item.kind, PatternKind::Wild) {
                        continue;
                    }
                    let element = self.b.build_extract_value(tuple, index as u32, "").must();
                    self.pattern(item, Some(element), fail);
                }
            }
            PatternKind::DynType { ty, inner } => {
                // Slot 0 of every vtable is the descriptor of the concrete type, so the object is
                // a `ty` when that is `ty`'s own descriptor; the data pointer is then the value.
                let object = value.unwrap().into_struct_value();
                let vtable = self.b.build_extract_value(object, 1, "").must().into_pointer_value();
                let found = self.vtable_slot(vtable, 0);
                let expected = self.desc(*ty);
                let i64_type = self.cx.i64_type();
                let found = self.b.build_ptr_to_int(found, i64_type, "").must();
                let expected = self.b.build_ptr_to_int(expected, i64_type, "").must();
                let same = self.b.build_int_compare(IntPredicate::EQ, found, expected, "").must();
                self.require(same, fail);
                if !matches!(inner.kind, PatternKind::Wild) {
                    let data = self.b.build_extract_value(object, 0, "").must();
                    self.pattern(inner, Some(data), fail);
                }
            }
            PatternKind::Or(alternatives) => {
                let success = self.block("alternative_matched");
                let (last, rest) = alternatives.split_last().expect("an `|` pattern has alternatives");
                for alternative in rest {
                    let next = self.block("next_alternative");
                    self.pattern(alternative, value, next);
                    self.b.build_unconditional_branch(success).must();
                    self.b.position_at_end(next);
                }
                self.pattern(last, value, fail);
                self.b.build_unconditional_branch(success).must();
                self.b.position_at_end(success);
            }
        }
    }
}
