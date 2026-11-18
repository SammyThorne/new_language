//! Built-in functions and the methods of `str`, `list`, `dict` and optionals.

use crate::{Codegen, Helper, Must};
use inkwell::IntPredicate;
use inkwell::values::BasicValueEnum;
use lang_sema::hir::{Builtin, Expr};
use lang_sema::types::TyKind;

impl<'ctx> Codegen<'ctx, '_> {
    pub(crate) fn builtin(&mut self, op: Builtin, args: &[Expr], expr: &Expr) -> Option<BasicValueEnum<'ctx>> {
        let span = expr.span;
        if op == Builtin::Print {
            let builder = self.builder_new();
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    self.push_text(builder, " ");
                }
                let value = self.expr(arg);
                self.push_value(builder, arg.ty, value, false);
            }
            self.push_text(builder, "\n");
            self.call_rt("lrt_builder_print", &[builder.into()]);
            return None;
        }
        if op == Builtin::ToStr {
            let value = self.expr(&args[0]);
            return Some(self.display_string(args[0].ty, value));
        }

        // Every other builtin has value-typed arguments, evaluated left to right.
        let values: Vec<BasicValueEnum<'ctx>> = args.iter().map(|a| self.value(a)).collect();
        let receiver_ty = args.first().map(|a| a.ty);
        let elem_ty = receiver_ty.and_then(|t| match self.kind(t) {
            TyKind::List(elem) => Some(*elem),
            _ => None,
        });
        let dict_types = receiver_ty.and_then(|t| match self.kind(t) {
            TyKind::Dict(key, value) => Some((*key, *value)),
            _ => None,
        });
        let int_to_bool = |this: &mut Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
            this.b.build_int_compare(IntPredicate::NE, v.into_int_value(), this.i64(0), "").must().into()
        };

        Some(match op {
            Builtin::Print | Builtin::ToStr => unreachable!("handled above"),
            Builtin::Panic => {
                let [ptr, len] = self.str_parts(values[0]);
                let (loc, loc_len) = self.location(span);
                self.call_rt("lrt_panic", &[ptr, len, loc.into(), loc_len.into()]);
                self.b.build_unreachable().must();
                self.start_dead_block();
                return None;
            }
            Builtin::Len => match self.kind(args[0].ty) {
                TyKind::Str => self.str_parts(values[0])[1],
                TyKind::List(_) => self.list_len(values[0].into_pointer_value()).into(),
                _ => self.dict_len(values[0].into_pointer_value()).into(),
            },
            Builtin::IntOfFloat => {
                let (loc, loc_len) = self.location(span);
                self.call_rt("lrt_float_to_int", &[values[0], loc.into(), loc_len.into()]).unwrap()
            }
            Builtin::IntOfBool => {
                self.b.build_int_z_extend(values[0].into_int_value(), self.cx.i64_type(), "").must().into()
            }
            Builtin::FloatOfInt => {
                self.b.build_signed_int_to_float(values[0].into_int_value(), self.cx.f64_type(), "").must().into()
            }
            Builtin::AbsInt => {
                let v = values[0].into_int_value();
                let negative = self.b.build_int_compare(IntPredicate::SLT, v, self.i64(0), "").must();
                let negated = self.b.build_int_neg(v, "").must();
                self.b.build_select(negative, negated, v, "").must()
            }
            Builtin::AbsFloat => self.call_rt("llvm.fabs.f64", &[values[0]]).unwrap(),

            // ---- str ----
            Builtin::StrUpper | Builtin::StrLower | Builtin::StrStrip | Builtin::StrLstrip | Builtin::StrRstrip => {
                let name = match op {
                    Builtin::StrUpper => "lrt_str_upper",
                    Builtin::StrLower => "lrt_str_lower",
                    Builtin::StrStrip => "lrt_str_strip",
                    Builtin::StrLstrip => "lrt_str_lstrip",
                    _ => "lrt_str_rstrip",
                };
                let [ptr, len] = self.str_parts(values[0]);
                self.call_rt(name, &[ptr, len]).unwrap()
            }
            Builtin::StrSplit => {
                let [ptr, len] = self.str_parts(values[0]);
                let [sep, sep_len] = self.str_parts(values[1]);
                let desc = self.desc(expr.ty);
                let (loc, loc_len) = self.location(span);
                self.call_rt("lrt_str_split", &[ptr, len, sep, sep_len, desc.into(), loc.into(), loc_len.into()])
                    .unwrap()
            }
            Builtin::StrSplitWhitespace => {
                let [ptr, len] = self.str_parts(values[0]);
                let desc = self.desc(expr.ty);
                self.call_rt("lrt_str_split_ws", &[ptr, len, desc.into()]).unwrap()
            }
            Builtin::StrJoin => {
                let [sep, sep_len] = self.str_parts(values[0]);
                self.call_rt("lrt_str_join", &[sep, sep_len, values[1]]).unwrap()
            }
            Builtin::StrReplace => {
                let [ptr, len] = self.str_parts(values[0]);
                let [old, old_len] = self.str_parts(values[1]);
                let [new, new_len] = self.str_parts(values[2]);
                self.call_rt("lrt_str_replace", &[ptr, len, old, old_len, new, new_len]).unwrap()
            }
            Builtin::StrFind | Builtin::StrContains | Builtin::StrStartsWith | Builtin::StrEndsWith => {
                let [ptr, len] = self.str_parts(values[0]);
                let [other, other_len] = self.str_parts(values[1]);
                let name = match op {
                    Builtin::StrStartsWith => "lrt_str_starts_with",
                    Builtin::StrEndsWith => "lrt_str_ends_with",
                    _ => "lrt_str_find",
                };
                let result = self.call_rt(name, &[ptr, len, other, other_len]).unwrap();
                match op {
                    Builtin::StrFind => result,
                    Builtin::StrContains => self
                        .b
                        .build_int_compare(IntPredicate::SGE, result.into_int_value(), self.i64(0), "")
                        .must()
                        .into(),
                    _ => int_to_bool(self, result),
                }
            }

            // ---- list ----
            Builtin::ListAppend => {
                self.list_push(values[0].into_pointer_value(), values[1]);
                return None;
            }
            Builtin::ListExtend => {
                self.call_rt("lrt_list_extend", &[values[0], values[1]]);
                return None;
            }
            Builtin::ListInsert => {
                let slot = self.call_rt("lrt_list_insert", &[values[0], values[1]]).unwrap().into_pointer_value();
                self.b.build_store(slot, values[2]).must();
                return None;
            }
            Builtin::ListPopLast | Builtin::ListPopAt => {
                let elem = elem_ty.unwrap();
                let out = self.alloca(self.value_type(elem), "popped");
                let index = if op == Builtin::ListPopAt { values[1] } else { self.i64(-1).into() };
                let (loc, loc_len) = self.location(span);
                self.call_rt("lrt_list_pop", &[values[0], index, out.into(), loc.into(), loc_len.into()]);
                self.b.build_load(self.value_type(elem), out, "").must()
            }
            Builtin::ListRemove | Builtin::ListIndex => {
                let elem = elem_ty.unwrap();
                let eq = self.helper(Helper::EqPtr, elem).as_global_value().as_pointer_value();
                let value = self.spill(values[1]);
                if op == Builtin::ListRemove {
                    let (loc, loc_len) = self.location(span);
                    self.call_rt(
                        "lrt_list_remove_value",
                        &[values[0], value.into(), eq.into(), loc.into(), loc_len.into()],
                    );
                    return None;
                }
                let index = self.call_rt("lrt_list_index_of", &[values[0], value.into(), eq.into()]).unwrap();
                let missing =
                    self.b.build_int_compare(IntPredicate::SLT, index.into_int_value(), self.i64(0), "").must();
                self.check(missing, |this| this.panic_message("index(): the value is not in the list", span));
                index
            }
            Builtin::ListClear | Builtin::ListReverse => {
                let name = if op == Builtin::ListClear { "lrt_list_clear" } else { "lrt_list_reverse" };
                self.call_rt(name, &[values[0]]);
                return None;
            }
            Builtin::ListSort => {
                let cmp = self.helper(Helper::CmpPtr, elem_ty.unwrap()).as_global_value().as_pointer_value();
                self.call_rt("lrt_list_sort", &[values[0], cmp.into()]);
                return None;
            }
            Builtin::ListCopy => self.call_rt("lrt_list_copy", &[values[0]]).unwrap(),

            // ---- dict ----
            Builtin::DictGet | Builtin::DictGetOr => {
                let (_, value_ty) = dict_types.unwrap();
                let key = self.spill(values[1]);
                let found = self.call_rt("lrt_dict_find", &[values[0], key.into()]).unwrap().into_pointer_value();
                let present = self.b.build_is_not_null(found, "").must();
                let value_type = self.value_type(value_ty);
                let load = move |this: &mut Self| Some(this.b.build_load(value_type, found, "").must());
                if op == Builtin::DictGet {
                    let option = expr.ty;
                    return self.choose(
                        present,
                        option,
                        |this| {
                            let value = load(this).unwrap();
                            Some(this.wrap_some(option, value))
                        },
                        |this| Some(this.none_value(option)),
                    );
                }
                let default = values[2];
                return self.choose(present, value_ty, load, |_| Some(default));
            }
            Builtin::DictPop => {
                let (_, value_ty) = dict_types.unwrap();
                let value_type = self.value_type(value_ty);
                let out = self.alloca(value_type, "popped");
                self.b.build_store(out, value_type.const_zero()).must();
                let key = self.spill(values[1]);
                let removed = self.call_rt("lrt_dict_remove", &[values[0], key.into(), out.into()]).unwrap();
                let present =
                    self.b.build_int_compare(IntPredicate::NE, removed.into_int_value(), self.i64(0), "").must();
                let option = expr.ty;
                return self.choose(
                    present,
                    option,
                    |this| {
                        let value = this.b.build_load(value_type, out, "").must();
                        Some(this.wrap_some(option, value))
                    },
                    |this| Some(this.none_value(option)),
                );
            }
            Builtin::DictKeys | Builtin::DictValues | Builtin::DictItems => {
                let (key_ty, value_ty) = dict_types.unwrap();
                let dict = values[0].into_pointer_value();
                let desc = self.desc(expr.ty);
                let len = self.dict_len(dict);
                let list = self.call_rt("lrt_list_new", &[desc.into(), len.into()]).unwrap().into_pointer_value();
                let cursor = self.alloca(self.cx.i64_type().into(), "cursor");
                self.b.build_store(cursor, self.i64(0)).must();
                let (header, body, exit) = (self.block("collect"), self.block("collect_body"), self.block("collected"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let entry = self.call_rt("lrt_dict_next", &[dict.into(), cursor.into()]).unwrap().into_pointer_value();
                let done = self.b.build_is_null(entry, "").must();
                self.b.build_conditional_branch(done, exit, body).must();
                self.b.position_at_end(body);
                let element = match op {
                    Builtin::DictKeys => self.entry_key(entry, key_ty),
                    Builtin::DictValues => self.entry_value(entry, key_ty, value_ty),
                    _ => {
                        let key = self.entry_key(entry, key_ty);
                        let value = self.entry_value(entry, key_ty, value_ty);
                        self.make_tuple(&[key, value])
                    }
                };
                self.list_push(list, element);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                list.into()
            }
            Builtin::DictClear => {
                self.call_rt("lrt_dict_clear", &[values[0]]);
                return None;
            }

            // ---- optionals ----
            Builtin::OptUnwrap => {
                let option = args[0].ty;
                let present = self.opt_is_some(option, values[0]);
                let missing = self.b.build_not(present, "").must();
                self.check(missing, |this| this.panic_message("unwrap() called on None", span));
                self.opt_inner(option, values[0])
            }
            Builtin::OptUnwrapOr => {
                let option = args[0].ty;
                let present = self.opt_is_some(option, values[0]);
                let inner = self.opt_inner(option, values[0]);
                let default = values[1];
                return self.choose(present, expr.ty, |_| Some(inner), |_| Some(default));
            }
            Builtin::OptIsSome | Builtin::OptIsNone => {
                let present = self.opt_is_some(args[0].ty, values[0]);
                if op == Builtin::OptIsNone { self.b.build_not(present, "").must().into() } else { present.into() }
            }

            // ---- Result ----
            Builtin::ResultUnwrap => {
                let result_ty = args[0].ty;
                let ok = self.result_is_ok(values[0]);
                let failed = self.b.build_not(ok, "").must();
                let value = values[0];
                self.check(failed, |this| {
                    let TyKind::Result(_, err_ty) = *this.kind(result_ty) else { unreachable!() };
                    let error = this.result_err(result_ty, value);
                    let builder = this.builder_new();
                    this.push_text(builder, "unwrap() called on an error: ");
                    this.push_error_text(builder, err_ty, error);
                    let message = this.call_rt("lrt_builder_finish", &[builder.into()]).unwrap();
                    let [ptr, len] = this.str_parts(message);
                    let (loc, loc_len) = this.location(span);
                    this.call_rt("lrt_panic", &[ptr, len, loc.into(), loc_len.into()]);
                });
                return self.result_ok(result_ty, value);
            }
            Builtin::ResultUnwrapOr => {
                let ok = self.result_is_ok(values[0]);
                let payload = self.result_ok(args[0].ty, values[0]);
                let default = values[1];
                return self.choose(ok, expr.ty, |_| payload, |_| Some(default));
            }
            Builtin::ResultUnwrapErr => {
                let ok = self.result_is_ok(values[0]);
                self.check(ok, |this| this.panic_message("unwrap_err() called on an Ok value", span));
                self.result_err(args[0].ty, values[0])
            }
            Builtin::ResultIsOk | Builtin::ResultIsErr => {
                let ok = self.result_is_ok(values[0]);
                if op == Builtin::ResultIsErr { self.b.build_not(ok, "").must().into() } else { ok.into() }
            }
            Builtin::ParseInt | Builtin::ParseFloat => {
                let (function, expected, number_type): (_, _, inkwell::types::BasicTypeEnum) =
                    if op == Builtin::ParseInt {
                        ("lrt_parse_int", "int", self.cx.i64_type().into())
                    } else {
                        ("lrt_parse_float", "float", self.cx.f64_type().into())
                    };
                let [ptr, len] = self.str_parts(values[0]);
                let out = self.alloca(number_type, "parsed");
                let parsed = self.call_rt(function, &[ptr, len, out.into()]).unwrap();
                let ok = self.b.build_int_compare(IntPredicate::NE, parsed.into_int_value(), self.i64(0), "").must();
                let result_ty = expr.ty;
                let text = values[0];
                return self.choose(
                    ok,
                    result_ty,
                    |this| {
                        let number = this.b.build_load(number_type, out, "").must();
                        Some(this.make_result(result_ty, true, Some(number)))
                    },
                    |this| {
                        let error = this.parse_error(text, expected);
                        Some(this.make_result(result_ty, false, Some(error)))
                    },
                );
            }
            Builtin::Copy => {
                // Enums are immutable, so a copy can be the same object.
                let TyKind::Struct(id) = *self.kind(args[0].ty) else { return Some(values[0]) };
                let struct_type = self.struct_types[id.0];
                let size = self.td.get_abi_size(&struct_type);
                let desc = self.desc(args[0].ty);
                let copy = self
                    .call_rt("lrt_alloc", &[self.i64(size as i64).into(), desc.into()])
                    .unwrap()
                    .into_pointer_value();
                self.b.build_memcpy(copy, 8, values[0].into_pointer_value(), 8, self.i64(size as i64)).must();
                copy.into()
            }
            Builtin::Hash => self.emit_hash(args[0].ty, values[0]).into(),
        })
    }
}
