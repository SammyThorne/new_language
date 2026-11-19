//! JSON encoders and decoders, generated per type (docs/05-standard-library.md, "JSON").
//!
//! For each type that is encoded, a helper `(builder, value)` appends its JSON text: field names
//! are pre-escaped constants, and the runtime writes strings and numbers. For each type that is
//! decoded, a helper `(reader, out) -> bool` drives the runtime's scanner and stores the value at
//! `out`; it returns false after the reader has recorded an error. Struct keys are matched against
//! the field names in the generated code, and values go straight into the struct's fields: there
//! is no intermediate tree and no lookup by name at run time.
//!
//! Composite types each get their own helper, which also handles recursive types. `json.Value`
//! is encoded as the JSON value it holds rather than as a tagged enum.

use crate::{Codegen, Helper, Must};
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use lang_sema::types::{Ty, TyKind};

/// A string as a JSON string literal, for keys known when compiling.
fn json_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl<'ctx> Codegen<'ctx, '_> {
    fn is_json_value(&self, ty: Ty) -> bool {
        match (self.program.json, self.kind(ty)) {
            (Some(json), TyKind::Enum(id)) => *id == json.value,
            _ => false,
        }
    }

    // ---- encoding -----------------------------------------------------------------------

    /// Append the JSON text of `value` (of type `ty`) to `builder`.
    pub(crate) fn emit_json(&mut self, builder: PointerValue<'ctx>, ty: Ty, value: Option<BasicValueEnum<'ctx>>) {
        match self.kind(ty).clone() {
            TyKind::None => self.push_text(builder, "null"),
            TyKind::Int => {
                self.call_rt("lrt_builder_push_int", &[builder.into(), value.unwrap()]);
            }
            TyKind::Float => {
                self.call_rt("lrt_builder_push_json_float", &[builder.into(), value.unwrap()]);
            }
            TyKind::Sized(kind) => {
                let wide = self.widen(kind, value.unwrap());
                let function = match (kind.is_float(), kind.is_signed()) {
                    (true, _) => "lrt_builder_push_json_float",
                    (false, true) => "lrt_builder_push_int",
                    (false, false) => "lrt_builder_push_uint",
                };
                self.call_rt(function, &[builder.into(), wide]);
            }
            TyKind::Bool => {
                let truth = self.bytes_const("true");
                let falsehood = self.bytes_const("false");
                let cond = value.unwrap().into_int_value();
                let ptr = self.b.build_select(cond, truth, falsehood, "").must();
                let len = self.b.build_select(cond, self.i64(4), self.i64(5), "").must();
                self.call_rt("lrt_builder_push", &[builder.into(), ptr, len]);
            }
            TyKind::Str => {
                let [ptr, len] = self.str_parts(value.unwrap());
                self.call_rt("lrt_builder_push_json_str", &[builder.into(), ptr, len]);
            }
            _ => {
                let helper = self.helper(Helper::JsonEncode, ty);
                let call = self.b.build_call(helper, &[builder.into(), value.unwrap().into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
            }
        }
    }

    pub(crate) fn json_encode_body(&mut self, function: FunctionValue<'ctx>, ty: Ty) {
        let builder = function.get_nth_param(0).unwrap().into_pointer_value();
        let value = function.get_nth_param(1).unwrap();
        if self.is_json_value(ty) {
            self.encode_json_value(builder, ty, value);
            self.b.build_return(None).must();
            return;
        }
        match self.kind(ty).clone() {
            TyKind::Option(inner) => {
                let present = self.opt_is_some(ty, value);
                let (some, none, done) = (self.block("some"), self.block("none"), self.block("done"));
                self.b.build_conditional_branch(present, some, none).must();
                self.b.position_at_end(some);
                let inner_value = self.opt_inner(ty, value);
                self.emit_json(builder, inner, Some(inner_value));
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(none);
                self.push_text(builder, "null");
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(done);
            }
            TyKind::List(elem) => {
                let list = value.into_pointer_value();
                self.push_text(builder, "[");
                self.list_loop(list, elem, None, |this, i, item| {
                    this.separator(builder, i);
                    this.emit_json(builder, elem, Some(item));
                });
                self.push_text(builder, "]");
            }
            TyKind::Tuple(items) => {
                self.push_text(builder, "[");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.push_text(builder, ",");
                    }
                    let element = self.b.build_extract_value(value.into_struct_value(), index as u32, "").must();
                    self.emit_json(builder, *item, Some(element));
                }
                self.push_text(builder, "]");
            }
            TyKind::Dict(key_ty, value_ty) => {
                let dict = value.into_pointer_value();
                self.push_text(builder, "{");
                let cursor = self.alloca(self.cx.i64_type().into(), "cursor");
                self.b.build_store(cursor, self.i64(0)).must();
                let count = self.alloca(self.cx.i64_type().into(), "count");
                self.b.build_store(count, self.i64(0)).must();
                let (header, body, exit) = (self.block("loop"), self.block("body"), self.block("end"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let entry = self.call_rt("lrt_dict_next", &[dict.into(), cursor.into()]).unwrap().into_pointer_value();
                let done = self.b.build_is_null(entry, "").must();
                self.b.build_conditional_branch(done, exit, body).must();
                self.b.position_at_end(body);
                let n = self.b.build_load(self.cx.i64_type(), count, "").must().into_int_value();
                self.separator(builder, n);
                let next = self.b.build_int_add(n, self.i64(1), "").must();
                self.b.build_store(count, next).must();
                let key = self.entry_key(entry, key_ty);
                self.emit_json(builder, key_ty, Some(key));
                self.push_text(builder, ":");
                let item = self.entry_value(entry, key_ty, value_ty);
                self.emit_json(builder, value_ty, Some(item));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                self.push_text(builder, "}");
            }
            TyKind::Struct(id) => {
                let fields: Vec<(String, Ty)> =
                    self.program.structs[id.0].fields.iter().map(|f| (f.json_name.clone(), f.ty)).collect();
                if fields.is_empty() {
                    self.push_text(builder, "{}");
                }
                for (index, (name, field_ty)) in fields.iter().enumerate() {
                    let open = if index == 0 { "{" } else { "," };
                    self.push_text(builder, &format!("{open}{}:", json_literal(name)));
                    let field = self.struct_field(ty, index, value);
                    self.emit_json(builder, *field_ty, Some(field));
                    if index + 1 == fields.len() {
                        self.push_text(builder, "}");
                    }
                }
            }
            TyKind::Enum(id) => {
                let def = &self.program.enums[id.0];
                let fieldless = def.variants.iter().all(|v| v.fields.is_empty());
                let done = self.block("encoded");
                self.for_each_variant(ty, value, done, |this, variant| {
                    let def = &this.program.enums[id.0].variants[variant];
                    let (name, fields): (String, Vec<(String, Ty)>) =
                        (def.name.clone(), def.fields.iter().map(|f| (f.json_name.clone(), f.ty)).collect());
                    if fieldless {
                        this.push_text(builder, &json_literal(&name));
                        return;
                    }
                    this.push_text(builder, &format!("{{\"type\":{}", json_literal(&name)));
                    for (index, (field_name, field_ty)) in fields.iter().enumerate() {
                        this.push_text(builder, &format!(",{}:", json_literal(field_name)));
                        let field = this.variant_field(ty, variant, index, value);
                        this.emit_json(builder, *field_ty, Some(field));
                    }
                    this.push_text(builder, "}");
                });
                self.b.position_at_end(done);
            }
            other => unreachable!("not JSON-capable: {other:?}"),
        }
        self.b.build_return(None).must();
    }

    /// `,` before every element but the first.
    fn separator(&mut self, builder: PointerValue<'ctx>, index: IntValue<'ctx>) {
        let later = self.b.build_int_compare(IntPredicate::NE, index, self.i64(0), "").must();
        let (comma, next) = (self.block("comma"), self.block("element"));
        self.b.build_conditional_branch(later, comma, next).must();
        self.b.position_at_end(comma);
        self.push_text(builder, ",");
        self.b.build_unconditional_branch(next).must();
        self.b.position_at_end(next);
    }

    /// `json.Value`: the JSON it stands for.
    fn encode_json_value(&mut self, builder: PointerValue<'ctx>, ty: Ty, value: BasicValueEnum<'ctx>) {
        let TyKind::Enum(id) = *self.kind(ty) else { unreachable!() };
        let done = self.block("encoded");
        self.for_each_variant(ty, value, done, |this, variant| {
            let def = &this.program.enums[id.0].variants[variant];
            if def.fields.is_empty() {
                this.push_text(builder, "null");
                return;
            }
            let field_ty = def.fields[0].ty;
            let payload = this.variant_field(ty, variant, 0, value);
            this.emit_json(builder, field_ty, Some(payload));
        });
        self.b.position_at_end(done);
    }

    // ---- decoding -----------------------------------------------------------------------

    /// Decode a value of type `ty` into `out`; the flag is false after an error.
    pub(crate) fn decode_json(
        &mut self,
        reader: PointerValue<'ctx>,
        ty: Ty,
        out: PointerValue<'ctx>,
    ) -> IntValue<'ctx> {
        let helper = self.helper(Helper::JsonDecode, ty);
        let call = self.b.build_call(helper, &[reader.into(), out.into()], "").must();
        call.set_call_convention(crate::CALLCONV_FAST);
        call.try_as_basic_value().basic().unwrap().into_int_value()
    }

    fn nonzero(&mut self, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        self.b.build_int_compare(IntPredicate::NE, value.into_int_value(), self.i64(0), "").must()
    }

    /// Continue if `ok`, otherwise return false from the helper being generated.
    fn or_fail(&mut self, ok: IntValue<'ctx>, fail: BasicBlock<'ctx>) {
        let next = self.block("ok");
        self.b.build_conditional_branch(ok, next, fail).must();
        self.b.position_at_end(next);
    }

    fn fail_message(&mut self, reader: PointerValue<'ctx>, message: &str, fail: BasicBlock<'ctx>) {
        let text = self.str_const(message);
        let [ptr, len] = self.str_parts(text.into());
        self.call_rt("lrt_json_fail", &[reader.into(), ptr, len]);
        self.b.build_unconditional_branch(fail).must();
    }

    fn fail_expected(&mut self, reader: PointerValue<'ctx>, what: &str) {
        let text = self.str_const(what);
        let [ptr, len] = self.str_parts(text.into());
        self.call_rt("lrt_json_fail_expected", &[reader.into(), ptr, len]);
    }

    /// Whether a runtime string equals a constant.
    fn str_is(&mut self, value: BasicValueEnum<'ctx>, text: &str) -> IntValue<'ctx> {
        let constant = self.str_const(text);
        let [a, al] = self.str_parts(value);
        let [b, bl] = self.str_parts(constant.into());
        self.str_eq(a, al, b, bl)
    }

    fn push_key(&mut self, reader: PointerValue<'ctx>, key: &str) {
        let text = self.str_const(key);
        let [ptr, len] = self.str_parts(text.into());
        self.call_rt("lrt_json_push_key", &[reader.into(), ptr, len]);
    }

    pub(crate) fn json_decode_body(&mut self, function: FunctionValue<'ctx>, ty: Ty) {
        let reader = function.get_nth_param(0).unwrap().into_pointer_value();
        let out = function.get_nth_param(1).unwrap().into_pointer_value();
        let fail = self.block("fail");
        if self.is_json_value(ty) {
            self.decode_json_value(reader, ty, out, fail);
        } else {
            self.decode_body(reader, ty, out, fail);
        }
        let truth = self.bool(true);
        self.b.build_return(Some(&truth)).must();
        self.b.position_at_end(fail);
        let falsehood = self.bool(false);
        self.b.build_return(Some(&falsehood)).must();
    }

    fn decode_body(&mut self, reader: PointerValue<'ctx>, ty: Ty, out: PointerValue<'ctx>, fail: BasicBlock<'ctx>) {
        let r: BasicValueEnum = reader.into();
        match self.kind(ty).clone() {
            TyKind::Int => {
                let ok = self.call_rt("lrt_json_read_int", &[r, out.into()]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
            }
            TyKind::Float => {
                let ok = self.call_rt("lrt_json_read_float", &[r, out.into()]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
            }
            TyKind::Sized(kind) => {
                let wide = if kind.is_float() { self.cx.f64_type().into() } else { self.cx.i64_type().into() };
                let slot = self.alloca(wide, "number");
                let function = if kind.is_float() { "lrt_json_read_float" } else { "lrt_json_read_int" };
                let ok = self.call_rt(function, &[r, slot.into()]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
                let number = self.b.build_load(wide, slot, "").must();
                let from = if kind.is_float() { Ty::FLOAT } else { Ty::INT };
                let value = self.num_convert(from, ty, number);
                self.b.build_store(out, value).must();
            }
            TyKind::Bool => {
                let flag = self.call_rt("lrt_json_read_bool", &[r]).unwrap().into_int_value();
                let ok = self.b.build_int_compare(IntPredicate::SGE, flag, self.i64(0), "").must();
                self.or_fail(ok, fail);
                let value = self.b.build_int_compare(IntPredicate::EQ, flag, self.i64(1), "").must();
                self.b.build_store(out, value).must();
            }
            TyKind::Str => {
                let text = self.call_rt("lrt_json_read_str", &[r]).unwrap();
                let ptr = self.b.build_extract_value(text.into_struct_value(), 0, "").must().into_pointer_value();
                let ok = self.b.build_is_not_null(ptr, "").must();
                self.or_fail(ok, fail);
                self.b.build_store(out, text).must();
            }
            TyKind::None => {
                let ok = self.call_rt("lrt_json_read_null", &[r]).unwrap();
                let ok = self.nonzero(ok);
                let (good, bad) = (self.block("null"), self.block("not_null"));
                self.b.build_conditional_branch(ok, good, bad).must();
                self.b.position_at_end(bad);
                self.fail_expected(reader, "null");
                self.b.build_unconditional_branch(fail).must();
                self.b.position_at_end(good);
            }
            TyKind::Option(inner) => {
                let is_null = self.call_rt("lrt_json_read_null", &[r]).unwrap();
                let is_null = self.nonzero(is_null);
                let (none, some, done) = (self.block("none"), self.block("some"), self.block("done"));
                self.b.build_conditional_branch(is_null, none, some).must();
                self.b.position_at_end(none);
                let empty = self.none_value(ty);
                self.b.build_store(out, empty).must();
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(some);
                let slot = self.alloca(self.value_type(inner), "inner");
                let ok = self.decode_json(reader, inner, slot);
                self.or_fail(ok, fail);
                let inner_value = self.b.build_load(self.value_type(inner), slot, "").must();
                let wrapped = self.wrap_some(ty, inner_value);
                self.b.build_store(out, wrapped).must();
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(done);
            }
            TyKind::List(elem) => {
                let ok = self.call_rt("lrt_json_array_start", &[r]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
                let desc = self.desc(ty);
                let list =
                    self.call_rt("lrt_list_new", &[desc.into(), self.i64(0).into()]).unwrap().into_pointer_value();
                let slot = self.alloca(self.value_type(elem), "element");
                let counter = self.alloca(self.cx.i64_type().into(), "index");
                self.b.build_store(counter, self.i64(0)).must();
                let (header, body, exit) = (self.block("loop"), self.block("body"), self.block("end"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let next = self.call_rt("lrt_json_array_next", &[r]).unwrap().into_int_value();
                let failed = self.b.build_int_compare(IntPredicate::SLT, next, self.i64(0), "").must();
                let more = self.b.build_int_compare(IntPredicate::SGT, next, self.i64(0), "").must();
                let check = self.block("check");
                self.b.build_conditional_branch(failed, fail, check).must();
                self.b.position_at_end(check);
                self.b.build_conditional_branch(more, body, exit).must();
                self.b.position_at_end(body);
                let i = self.b.build_load(self.cx.i64_type(), counter, "").must();
                self.call_rt("lrt_json_push_index", &[r, i]);
                let ok = self.decode_json(reader, elem, slot);
                self.or_fail(ok, fail);
                self.call_rt("lrt_json_pop", &[r]);
                let item = self.b.build_load(self.value_type(elem), slot, "").must();
                self.list_push(list, item);
                let next_i = self.b.build_int_add(i.into_int_value(), self.i64(1), "").must();
                self.b.build_store(counter, next_i).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                self.b.build_store(out, list).must();
            }
            TyKind::Dict(key_ty, value_ty) => {
                let ok = self.call_rt("lrt_json_object_start", &[r]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
                let desc = self.desc(ty);
                let dict = self.call_rt("lrt_dict_new", &[desc.into()]).unwrap().into_pointer_value();
                let slot = self.alloca(self.value_type(value_ty), "value");
                let (header, body, exit) = (self.block("loop"), self.block("body"), self.block("end"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let key = self.object_key(reader, fail, body, exit);
                self.b.position_at_end(body);
                let [kp, kl] = self.str_parts(key);
                self.call_rt("lrt_json_push_key", &[r, kp, kl]);
                let ok = self.decode_json(reader, value_ty, slot);
                self.or_fail(ok, fail);
                self.call_rt("lrt_json_pop", &[r]);
                let key_ptr = self.spill(key);
                let _ = key_ty;
                let target =
                    self.call_rt("lrt_dict_slot", &[dict.into(), key_ptr.into()]).unwrap().into_pointer_value();
                let item = self.b.build_load(self.value_type(value_ty), slot, "").must();
                self.b.build_store(target, item).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                self.b.build_store(out, dict).must();
            }
            TyKind::Tuple(items) => {
                let ok = self.call_rt("lrt_json_array_start", &[r]).unwrap();
                let ok = self.nonzero(ok);
                self.or_fail(ok, fail);
                let mut values = Vec::new();
                for (index, item) in items.iter().enumerate() {
                    let next = self.call_rt("lrt_json_array_next", &[r]).unwrap().into_int_value();
                    let more = self.b.build_int_compare(IntPredicate::EQ, next, self.i64(1), "").must();
                    let (good, short) = (self.block("element"), self.block("short"));
                    self.b.build_conditional_branch(more, good, short).must();
                    self.b.position_at_end(short);
                    self.fail_message(reader, &format!("expected an array of {} elements", items.len()), fail);
                    self.b.position_at_end(good);
                    let slot = self.alloca(self.value_type(*item), "element");
                    self.call_rt("lrt_json_push_index", &[r, self.i64(index as i64).into()]);
                    let ok = self.decode_json(reader, *item, slot);
                    self.or_fail(ok, fail);
                    self.call_rt("lrt_json_pop", &[r]);
                    values.push(self.b.build_load(self.value_type(*item), slot, "").must());
                }
                let next = self.call_rt("lrt_json_array_next", &[r]).unwrap().into_int_value();
                let end = self.b.build_int_compare(IntPredicate::EQ, next, self.i64(0), "").must();
                let (good, long) = (self.block("end"), self.block("long"));
                self.b.build_conditional_branch(end, good, long).must();
                self.b.position_at_end(long);
                self.fail_message(reader, &format!("expected an array of {} elements", items.len()), fail);
                self.b.position_at_end(good);
                let tuple = self.make_tuple(&values);
                self.b.build_store(out, tuple).must();
            }
            TyKind::Struct(id) => {
                let fields: Vec<(String, Ty)> =
                    self.program.structs[id.0].fields.iter().map(|f| (f.json_name.clone(), f.ty)).collect();
                let strict = self.program.structs[id.0].json_strict;
                let values = self.decode_fields(reader, ty, &fields, None, strict, fail, |this, index| {
                    let TyKind::Struct(id) = *this.kind(ty) else { unreachable!() };
                    this.program.structs[id.0].fields[index].default.clone()
                });
                let object = self.new_struct(ty, &values);
                self.b.build_store(out, object).must();
            }
            TyKind::Enum(id) => {
                let variants: Vec<(String, Vec<(String, Ty)>)> = self.program.enums[id.0]
                    .variants
                    .iter()
                    .map(|v| (v.name.clone(), v.fields.iter().map(|f| (f.json_name.clone(), f.ty)).collect()))
                    .collect();
                let fieldless = variants.iter().all(|(_, fields)| fields.is_empty());
                let tag = if fieldless {
                    let text = self.call_rt("lrt_json_read_str", &[r]).unwrap();
                    let ptr = self.b.build_extract_value(text.into_struct_value(), 0, "").must().into_pointer_value();
                    let ok = self.b.build_is_not_null(ptr, "").must();
                    self.or_fail(ok, fail);
                    text
                } else {
                    let key = self.str_const("type");
                    let [kp, kl] = self.str_parts(key.into());
                    let text = self.call_rt("lrt_json_peek_tag", &[r, kp, kl]).unwrap();
                    let ptr = self.b.build_extract_value(text.into_struct_value(), 0, "").must().into_pointer_value();
                    let ok = self.b.build_is_not_null(ptr, "").must();
                    let (good, bad) = (self.block("tagged"), self.block("untagged"));
                    self.b.build_conditional_branch(ok, good, bad).must();
                    self.b.position_at_end(bad);
                    self.fail_expected(reader, "an object with a string \"type\"");
                    self.b.build_unconditional_branch(fail).must();
                    self.b.position_at_end(good);
                    text
                };
                let done = self.block("decoded");
                for (index, (name, fields)) in variants.iter().enumerate() {
                    let matches = self.str_is(tag, name);
                    let (this_variant, next) = (self.block("variant"), self.block("next_variant"));
                    self.b.build_conditional_branch(matches, this_variant, next).must();
                    self.b.position_at_end(this_variant);
                    let value = if fieldless {
                        self.variant_values(ty, index, Vec::new())
                    } else {
                        let values = self.decode_fields(reader, ty, fields, Some("type"), false, fail, |_, _| None);
                        self.variant_values(ty, index, values)
                    };
                    self.b.build_store(out, value).must();
                    self.b.build_unconditional_branch(done).must();
                    self.b.position_at_end(next);
                }
                let names: Vec<String> = variants.iter().map(|(n, _)| format!("\"{n}\"")).collect();
                self.fail_message(reader, &format!("unknown variant; expected one of {}", names.join(", ")), fail);
                self.b.position_at_end(done);
            }
            other => unreachable!("not JSON-capable: {other:?}"),
        }
    }

    /// Read the next key of the open object: branch to `body` with the key, or to `exit` at the
    /// end of the object, or to `fail`. Leaves the builder in `body`'s predecessor; returns the key.
    fn object_key(
        &mut self,
        reader: PointerValue<'ctx>,
        fail: BasicBlock<'ctx>,
        body: BasicBlock<'ctx>,
        exit: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let key = self.call_rt("lrt_json_object_key", &[reader.into()]).unwrap();
        let ptr = self.b.build_extract_value(key.into_struct_value(), 0, "").must().into_pointer_value();
        let present = self.b.build_is_not_null(ptr, "").must();
        let end = self.block("object_end");
        self.b.build_conditional_branch(present, body, end).must();
        self.b.position_at_end(end);
        let failed = self.call_rt("lrt_json_failed", &[reader.into()]).unwrap();
        let failed = self.nonzero(failed);
        self.b.build_conditional_branch(failed, fail, exit).must();
        key
    }

    /// Read an object into one value per field (a struct's, or a variant's): keys may come in
    /// any order; unknown keys are skipped (or, `strict`, rejected); a missing field takes its
    /// default, or `None` if it is optional, or is an error. `skip_key` is ignored (an enum's
    /// `"type"`).
    #[allow(clippy::too_many_arguments)]
    fn decode_fields(
        &mut self,
        reader: PointerValue<'ctx>,
        owner: Ty,
        fields: &[(String, Ty)],
        skip_key: Option<&str>,
        strict: bool,
        fail: BasicBlock<'ctx>,
        default: impl Fn(&Self, usize) -> Option<lang_sema::hir::Expr>,
    ) -> Vec<BasicValueEnum<'ctx>> {
        let _ = owner;
        let r: BasicValueEnum = reader.into();
        let ok = self.call_rt("lrt_json_object_start", &[r]).unwrap();
        let ok = self.nonzero(ok);
        self.or_fail(ok, fail);
        let slots: Vec<PointerValue> = fields.iter().map(|(_, t)| self.alloca(self.value_type(*t), "field")).collect();
        let seen: Vec<PointerValue> = fields.iter().map(|_| self.alloca(self.cx.bool_type().into(), "seen")).collect();
        for flag in &seen {
            self.b.build_store(*flag, self.bool(false)).must();
        }
        let (header, body, exit) = (self.block("loop"), self.block("key"), self.block("end"));
        self.b.build_unconditional_branch(header).must();
        self.b.position_at_end(header);
        let key = self.object_key(reader, fail, body, exit);
        self.b.position_at_end(body);
        for (index, (name, field_ty)) in fields.iter().enumerate() {
            let matches = self.str_is(key, name);
            let (this_field, next) = (self.block("field"), self.block("next_field"));
            self.b.build_conditional_branch(matches, this_field, next).must();
            self.b.position_at_end(this_field);
            self.push_key(reader, name);
            let ok = self.decode_json(reader, *field_ty, slots[index]);
            self.or_fail(ok, fail);
            self.call_rt("lrt_json_pop", &[r]);
            self.b.build_store(seen[index], self.bool(true)).must();
            self.b.build_unconditional_branch(header).must();
            self.b.position_at_end(next);
        }
        if let Some(skip) = skip_key {
            let matches = self.str_is(key, skip);
            let (skipped, other) = (self.block("skip"), self.block("other"));
            self.b.build_conditional_branch(matches, skipped, other).must();
            self.b.position_at_end(skipped);
            let ok = self.call_rt("lrt_json_skip_value", &[r]).unwrap();
            let ok = self.nonzero(ok);
            self.or_fail(ok, fail);
            self.b.build_unconditional_branch(header).must();
            self.b.position_at_end(other);
        }
        if strict {
            // Name the key in the message.
            let builder = self.builder_new();
            self.push_text(builder, "unknown key ");
            self.push_value(builder, Ty::STR, Some(key), true);
            let message = self.call_rt("lrt_builder_finish", &[builder.into()]).unwrap();
            let [mp, ml] = self.str_parts(message);
            self.call_rt("lrt_json_fail", &[r, mp, ml]);
            self.b.build_unconditional_branch(fail).must();
        } else {
            let ok = self.call_rt("lrt_json_skip_value", &[r]).unwrap();
            let ok = self.nonzero(ok);
            self.or_fail(ok, fail);
            self.b.build_unconditional_branch(header).must();
        }
        self.b.position_at_end(exit);
        let mut values = Vec::new();
        for (index, (name, field_ty)) in fields.iter().enumerate() {
            let flag = self.b.build_load(self.cx.bool_type(), seen[index], "").must().into_int_value();
            let (present, missing, done) = (self.block("present"), self.block("missing"), self.block("filled"));
            self.b.build_conditional_branch(flag, present, missing).must();
            self.b.position_at_end(missing);
            match (default(self, index), self.kind(*field_ty)) {
                (Some(expr), _) => {
                    if let Some(value) = self.expr(&expr) {
                        self.b.build_store(slots[index], value).must();
                    }
                    self.b.build_unconditional_branch(done).must();
                }
                (None, TyKind::Option(_)) => {
                    let none = self.none_value(*field_ty);
                    self.b.build_store(slots[index], none).must();
                    self.b.build_unconditional_branch(done).must();
                }
                (None, _) => self.fail_message(reader, &format!("missing key \"{name}\""), fail),
            }
            self.b.position_at_end(present);
            self.b.build_unconditional_branch(done).must();
            self.b.position_at_end(done);
            values.push(self.b.build_load(self.value_type(*field_ty), slots[index], "").must());
        }
        values
    }

    /// `json.Value`: whatever comes next.
    fn decode_json_value(
        &mut self,
        reader: PointerValue<'ctx>,
        ty: Ty,
        out: PointerValue<'ctx>,
        fail: BasicBlock<'ctx>,
    ) {
        let r: BasicValueEnum = reader.into();
        let TyKind::Enum(id) = *self.kind(ty) else { unreachable!() };
        let variant_index = |this: &Self, name: &str| {
            this.program.enums[id.0].variants.iter().position(|v| v.name == name).expect("json.Value variant")
        };
        let payload_ty = |this: &Self, name: &str| {
            let index = variant_index(this, name);
            this.program.enums[id.0].variants[index].fields[0].ty
        };
        let kind = self.call_rt("lrt_json_peek_kind", &[r]).unwrap().into_int_value();
        let done = self.block("decoded");
        let blocks: Vec<BasicBlock> = (0..6).map(|_| self.block("kind")).collect();
        let cases: Vec<_> = blocks.iter().enumerate().map(|(i, b)| (self.i64(i as i64), *b)).collect();
        self.b.build_switch(kind, fail, &cases).must();
        for (kind, block) in blocks.into_iter().enumerate() {
            self.b.position_at_end(block);
            let value = match kind {
                0 => {
                    self.call_rt("lrt_json_read_null", &[r]);
                    self.variant_values(ty, variant_index(self, "Null"), Vec::new())
                }
                1 => {
                    let slot = self.alloca(self.cx.bool_type().into(), "bool");
                    let ok = self.decode_json(reader, Ty::BOOL, slot);
                    self.or_fail(ok, fail);
                    let value = self.b.build_load(self.cx.bool_type(), slot, "").must();
                    self.variant_values(ty, variant_index(self, "Bool"), vec![value])
                }
                2 => {
                    let int_slot = self.alloca(self.cx.i64_type().into(), "int");
                    let float_slot = self.alloca(self.cx.f64_type().into(), "float");
                    let which = self
                        .call_rt("lrt_json_read_number", &[r, int_slot.into(), float_slot.into()])
                        .unwrap()
                        .into_int_value();
                    let ok = self.b.build_int_compare(IntPredicate::NE, which, self.i64(0), "").must();
                    self.or_fail(ok, fail);
                    let is_int = self.b.build_int_compare(IntPredicate::EQ, which, self.i64(1), "").must();
                    let (int_block, float_block) = (self.block("int"), self.block("float"));
                    self.b.build_conditional_branch(is_int, int_block, float_block).must();
                    self.b.position_at_end(int_block);
                    let value = self.b.build_load(self.cx.i64_type(), int_slot, "").must();
                    let int_value = self.variant_values(ty, variant_index(self, "Int"), vec![value]);
                    self.b.build_store(out, int_value).must();
                    self.b.build_unconditional_branch(done).must();
                    self.b.position_at_end(float_block);
                    let value = self.b.build_load(self.cx.f64_type(), float_slot, "").must();
                    self.variant_values(ty, variant_index(self, "Float"), vec![value])
                }
                3 => {
                    let slot = self.alloca(self.str_type.into(), "str");
                    let ok = self.decode_json(reader, Ty::STR, slot);
                    self.or_fail(ok, fail);
                    let value = self.b.build_load(self.str_type, slot, "").must();
                    self.variant_values(ty, variant_index(self, "Str"), vec![value])
                }
                4 | 5 => {
                    let name = if kind == 4 { "Array" } else { "Object" };
                    let collection = payload_ty(self, name);
                    let slot = self.alloca(self.ptr_type().into(), "collection");
                    let ok = self.decode_json(reader, collection, slot);
                    self.or_fail(ok, fail);
                    let value = self.b.build_load(self.ptr_type(), slot, "").must();
                    self.variant_values(ty, variant_index(self, name), vec![value])
                }
                _ => unreachable!(),
            };
            self.b.build_store(out, value).must();
            self.b.build_unconditional_branch(done).must();
        }
        self.b.position_at_end(done);
    }

    // ---- the json module's intrinsics ---------------------------------------------------

    /// `__json_encode(value)`.
    pub(crate) fn json_encode(&mut self, ty: Ty, value: Option<BasicValueEnum<'ctx>>) -> BasicValueEnum<'ctx> {
        let builder = self.builder_new();
        self.emit_json(builder, ty, value);
        self.call_rt("lrt_builder_finish", &[builder.into()]).unwrap()
    }

    /// `__json_decode(text)` into a `Result[T, JsonError]`.
    pub(crate) fn json_decode(&mut self, result_ty: Ty, text: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let TyKind::Result(target, _) = *self.kind(result_ty) else { unreachable!("decoding gives a Result") };
        let [ptr, len] = self.str_parts(text);
        let reader = self.call_rt("lrt_json_reader_new", &[ptr, len]).unwrap().into_pointer_value();
        let slot = self.alloca(self.llty(target).unwrap_or(self.cx.i64_type().into()), "decoded");
        let decoded = self.decode_json(reader, target, slot);
        let finished = self.call_rt("lrt_json_finish", &[reader.into()]).unwrap();
        let finished = self.nonzero(finished);
        let ok = self.b.build_and(decoded, finished, "").must();
        let result = self.choose(
            ok,
            result_ty,
            |this| {
                let value = this.llty(target).map(|t| this.b.build_load(t, slot, "").must());
                Some(this.make_result(result_ty, true, value))
            },
            |this| {
                let message = this.call_rt("lrt_json_error_message", &[reader.into()]).unwrap();
                let path = this.call_rt("lrt_json_error_path", &[reader.into()]).unwrap();
                let json = this.program.json.expect("decoding needs the json module");
                let error_ty = this.program.structs[json.error.0].ty;
                let error = this.new_struct(error_ty, &[message, path]);
                Some(this.make_result(result_ty, false, Some(error.into())))
            },
        );
        // The reader is Rust heap memory, which the collector does not scan, so its pointer to
        // the text does not keep the text alive. Passing the text to `free` does: it stays in a
        // scanned register or stack slot until the reader is gone.
        self.call_rt("lrt_json_reader_free", &[reader.into(), ptr]);
        result.expect("a result value")
    }
}
