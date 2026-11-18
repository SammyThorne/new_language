//! Helper functions generated per type, on first use: equality, hashing and ordering (the
//! built-in `Eq`, `Hash` and `Ord` implementations, monomorphised), and printing.
//!
//! The `...Ptr` variants take pointers and use the C calling convention, because the runtime calls
//! them for dict keys, `in`, `remove` and `sort`.

use crate::{Codegen, Helper, Must};
use inkwell::attributes::AttributeLoc;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};
use lang_sema::types::{Ty, TyKind};

impl<'ctx> Codegen<'ctx, '_> {
    pub(crate) fn helper(&mut self, helper: Helper, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&function) = self.helpers.get(&(helper, ty)) {
            return function;
        }
        let ptr = self.ptr_type();
        let i64_type = self.cx.i64_type();
        let value = self.value_type(ty);
        let name = self.program.types.display(ty);
        let (label, fn_type, fast) = match helper {
            Helper::Eq => ("eq", self.cx.bool_type().fn_type(&[value.into(), value.into()], false), true),
            Helper::Hash => ("hash", i64_type.fn_type(&[value.into()], false), true),
            Helper::Cmp => ("cmp", i64_type.fn_type(&[value.into(), value.into()], false), true),
            Helper::EqPtr => ("eq_ptr", i64_type.fn_type(&[ptr.into(), ptr.into()], false), false),
            Helper::HashPtr => ("hash_ptr", i64_type.fn_type(&[ptr.into()], false), false),
            Helper::CmpPtr => ("cmp_ptr", i64_type.fn_type(&[ptr.into(), ptr.into()], false), false),
            Helper::Push { repr } => {
                let params: [BasicMetadataTypeEnum; 2] = [ptr.into(), value.into()];
                (if repr { "repr" } else { "display" }, self.cx.void_type().fn_type(&params, false), true)
            }
        };
        let function = self.module.add_function(&format!("{label}.{name}"), fn_type, Some(Linkage::Private));
        function.add_attribute(AttributeLoc::Function, self.nounwind);
        if fast {
            function.set_call_conventions(crate::CALLCONV_FAST);
        }
        // Registered before the body is generated, so a helper may use itself for nested types.
        self.helpers.insert((helper, ty), function);
        self.in_function(function, |this| match helper {
            Helper::Eq => this.eq_body(function, ty),
            Helper::Hash => {
                let v = function.get_nth_param(0).unwrap();
                let hash = this.derived_hash(ty, v);
                this.b.build_return(Some(&hash)).must();
            }
            Helper::EqPtr => {
                let a = this.load_param(function, 0, ty);
                let b = this.load_param(function, 1, ty);
                let eq = this.emit_eq(ty, a, b);
                let result = this.b.build_int_z_extend(eq, this.cx.i64_type(), "").must();
                this.b.build_return(Some(&result)).must();
            }
            Helper::HashPtr => {
                let v = this.load_param(function, 0, ty);
                let hash = this.emit_hash(ty, v);
                this.b.build_return(Some(&hash)).must();
            }
            Helper::CmpPtr => {
                let a = this.load_param(function, 0, ty);
                let b = this.load_param(function, 1, ty);
                let cmp = this.emit_cmp(ty, a, b);
                this.b.build_return(Some(&cmp)).must();
            }
            Helper::Push { repr } => this.push_body(function, ty, repr),
            Helper::Cmp => this.cmp_body(function, ty),
        });
        function
    }

    fn load_param(&mut self, function: FunctionValue<'ctx>, index: u32, ty: Ty) -> BasicValueEnum<'ctx> {
        let ptr = function.get_nth_param(index).unwrap().into_pointer_value();
        self.b.build_load(self.value_type(ty), ptr, "").must()
    }

    fn eq_body(&mut self, function: FunctionValue<'ctx>, ty: Ty) {
        let a = function.get_nth_param(0).unwrap();
        let b = function.get_nth_param(1).unwrap();
        let result: IntValue = match self.kind(ty).clone() {
            TyKind::Tuple(items) => {
                let mut all = self.bool(true);
                for (index, item) in items.iter().enumerate() {
                    let x = self.b.build_extract_value(a.into_struct_value(), index as u32, "").must();
                    let y = self.b.build_extract_value(b.into_struct_value(), index as u32, "").must();
                    let eq = self.emit_eq(*item, x, y);
                    all = self.b.build_and(all, eq, "").must();
                }
                all
            }
            TyKind::List(elem) => {
                let eq = self.helper(Helper::EqPtr, elem).as_global_value().as_pointer_value();
                let result = self.call_rt("lrt_list_eq", &[a, b, eq.into()]).unwrap().into_int_value();
                self.b.build_int_compare(IntPredicate::NE, result, self.i64(0), "").must()
            }
            TyKind::Option(inner) => {
                // Equal if both are None, or both are present with equal values.
                let a_some = self.opt_is_some(ty, a);
                let b_some = self.opt_is_some(ty, b);
                let both = self.b.build_and(a_some, b_some, "").must();
                let same_presence = self.b.build_int_compare(IntPredicate::EQ, a_some, b_some, "").must();
                let start = self.b.get_insert_block().unwrap();
                let compare = self.block("compare");
                let done = self.block("done");
                self.b.build_conditional_branch(both, compare, done).must();
                self.b.position_at_end(compare);
                let x = self.opt_inner(ty, a);
                let y = self.opt_inner(ty, b);
                let inner_eq = self.emit_eq(inner, x, y);
                let compare_end = self.b.get_insert_block().unwrap();
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(done);
                let phi = self.b.build_phi(self.cx.bool_type(), "").must();
                phi.add_incoming(&[(&same_presence as &dyn BasicValue, start), (&inner_eq, compare_end)]);
                phi.as_basic_value().into_int_value()
            }
            TyKind::Struct(id) => {
                let mut all = self.bool(true);
                for index in 0..self.program.structs[id.0].fields.len() {
                    let field_ty = self.program.structs[id.0].fields[index].ty;
                    let x = self.struct_field(ty, index, a);
                    let y = self.struct_field(ty, index, b);
                    let eq = self.emit_eq(field_ty, x, y);
                    all = self.b.build_and(all, eq, "").must();
                }
                all
            }
            TyKind::Enum(id) => {
                // Equal tags, then equal fields for that variant.
                let result = self.alloca(self.cx.bool_type().into(), "equal");
                self.b.build_store(result, self.bool(false)).must();
                let tag_a = self.enum_tag(ty, a);
                let tag_b = self.enum_tag(ty, b);
                let same = self.b.build_int_compare(IntPredicate::EQ, tag_a, tag_b, "").must();
                let compare = self.block("same_variant");
                let done = self.block("done");
                self.b.build_conditional_branch(same, compare, done).must();
                self.b.position_at_end(compare);
                self.for_each_variant(ty, a, done, |this, variant| {
                    let mut all = this.bool(true);
                    for field in 0..this.program.enums[id.0].variants[variant].fields.len() {
                        let field_ty = this.program.enums[id.0].variants[variant].fields[field].ty;
                        let x = this.variant_field(ty, variant, field, a);
                        let y = this.variant_field(ty, variant, field, b);
                        let eq = this.emit_eq(field_ty, x, y);
                        all = this.b.build_and(all, eq, "").must();
                    }
                    this.b.build_store(result, all).must();
                });
                self.b.position_at_end(done);
                self.b.build_load(self.cx.bool_type(), result, "").must().into_int_value()
            }
            TyKind::Result(ok_ty, err_ty) => {
                let ok_a = self.result_is_ok(a);
                let ok_b = self.result_is_ok(b);
                let same = self.b.build_int_compare(IntPredicate::EQ, ok_a, ok_b, "").must();
                let ok_payloads = match (self.result_ok(ty, a), self.result_ok(ty, b)) {
                    (Some(x), Some(y)) => self.emit_eq(ok_ty, x, y),
                    _ => self.bool(true),
                };
                let (ea, eb) = (self.result_err(ty, a), self.result_err(ty, b));
                let err_payloads = self.emit_eq(err_ty, ea, eb);
                let payloads = self.b.build_select(ok_a, ok_payloads, err_payloads, "").must().into_int_value();
                self.b.build_and(same, payloads, "").must()
            }
            other => unreachable!("no equality helper for {other:?}"),
        };
        self.b.build_return(Some(&result)).must();
    }

    pub(crate) fn emit_hash(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        match self.kind(ty).clone() {
            TyKind::Struct(_) | TyKind::Enum(_) => {
                let hash_trait = self.program.builtins.hash;
                if let Some(function) = self.explicit_method(hash_trait, ty, 0) {
                    return self.call_method(function, &[value]).unwrap().into_int_value();
                }
                let helper = self.helper(Helper::Hash, ty);
                let call = self.b.build_call(helper, &[value.into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
                call.try_as_basic_value().basic().unwrap().into_int_value()
            }
            TyKind::Int => value.into_int_value(),
            TyKind::Sized(kind) => self.widen(kind, value).into_int_value(),
            TyKind::Bool => self.b.build_int_z_extend(value.into_int_value(), self.cx.i64_type(), "").must(),
            TyKind::Str => {
                let [ptr, len] = self.str_parts(value);
                self.call_rt("lrt_str_hash", &[ptr, len]).unwrap().into_int_value()
            }
            TyKind::Tuple(items) => {
                // The runtime mixes the final hash, so a simple combination is enough here.
                let mut hash = self.i64(0x345678);
                for (index, item) in items.iter().enumerate() {
                    let element = self.b.build_extract_value(value.into_struct_value(), index as u32, "").must();
                    let element_hash = self.emit_hash(*item, element);
                    let mixed = self.b.build_xor(hash, element_hash, "").must();
                    hash = self.b.build_int_mul(mixed, self.i64(1_000_003), "").must();
                }
                hash
            }
            other => unreachable!("cannot hash {other:?}"),
        }
    }

    /// A derived `Hash`: the fields' hashes combined, with the tag for enums.
    fn derived_hash(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let combine = |this: &mut Self, hash: IntValue<'ctx>, part: IntValue<'ctx>| {
            let mixed = this.b.build_xor(hash, part, "").must();
            this.b.build_int_mul(mixed, this.i64(1_000_003), "").must()
        };
        match self.kind(ty).clone() {
            TyKind::Struct(id) => {
                let mut hash = self.i64(0x345678);
                for index in 0..self.program.structs[id.0].fields.len() {
                    let field_ty = self.program.structs[id.0].fields[index].ty;
                    let field = self.struct_field(ty, index, value);
                    let part = self.emit_hash(field_ty, field);
                    hash = combine(self, hash, part);
                }
                hash
            }
            TyKind::Enum(id) => {
                let slot = self.alloca(self.cx.i64_type().into(), "hash");
                let tag = self.enum_tag(ty, value);
                let start = combine(self, self.i64(0x345678), tag);
                self.b.build_store(slot, start).must();
                let done = self.block("hashed");
                self.for_each_variant(ty, value, done, |this, variant| {
                    let mut hash = this.b.build_load(this.cx.i64_type(), slot, "").must().into_int_value();
                    for field in 0..this.program.enums[id.0].variants[variant].fields.len() {
                        let field_ty = this.program.enums[id.0].variants[variant].fields[field].ty;
                        let v = this.variant_field(ty, variant, field, value);
                        let part = this.emit_hash(field_ty, v);
                        hash = combine(this, hash, part);
                    }
                    this.b.build_store(slot, hash).must();
                });
                self.b.position_at_end(done);
                self.b.build_load(self.cx.i64_type(), slot, "").must().into_int_value()
            }
            other => unreachable!("no derived hash for {other:?}"),
        }
    }

    pub(crate) fn struct_field(&mut self, ty: Ty, index: usize, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let TyKind::Struct(id) = *self.kind(ty) else { unreachable!("not a struct") };
        let field_ty = self.program.structs[id.0].fields[index].ty;
        let ptr =
            self.b.build_struct_gep(self.struct_types[id.0], value.into_pointer_value(), index as u32 + 1, "").must();
        self.b.build_load(self.value_type(field_ty), ptr, "").must()
    }

    /// Run `body` once per variant, in a block reached by switching on the tag of `value`, then
    /// continue at `done`. The builder ends positioned nowhere useful; position it at `done`.
    pub(crate) fn for_each_variant(
        &mut self,
        ty: Ty,
        value: BasicValueEnum<'ctx>,
        done: inkwell::basic_block::BasicBlock<'ctx>,
        mut body: impl FnMut(&mut Self, usize),
    ) {
        let TyKind::Enum(id) = *self.kind(ty) else { unreachable!("not an enum") };
        let tag = self.enum_tag(ty, value);
        let count = self.program.enums[id.0].variants.len();
        let blocks: Vec<_> = (0..count).map(|_| self.block("variant")).collect();
        let impossible = self.block("bad_tag");
        let cases: Vec<_> = blocks.iter().enumerate().map(|(i, block)| (self.i64(i as i64), *block)).collect();
        self.b.build_switch(tag, impossible, &cases).must();
        self.b.position_at_end(impossible);
        self.b.build_unreachable().must();
        for (variant, block) in blocks.into_iter().enumerate() {
            self.b.position_at_end(block);
            body(self, variant);
            self.b.build_unconditional_branch(done).must();
        }
    }

    /// Three-way comparison: negative, zero or positive.
    pub(crate) fn emit_cmp(&mut self, ty: Ty, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let (less, greater) = match self.kind(ty) {
            TyKind::Int => (
                self.b.build_int_compare(IntPredicate::SLT, a.into_int_value(), b.into_int_value(), "").must(),
                self.b.build_int_compare(IntPredicate::SGT, a.into_int_value(), b.into_int_value(), "").must(),
            ),
            TyKind::Bool => (
                self.b.build_int_compare(IntPredicate::ULT, a.into_int_value(), b.into_int_value(), "").must(),
                self.b.build_int_compare(IntPredicate::UGT, a.into_int_value(), b.into_int_value(), "").must(),
            ),
            TyKind::Float => (
                self.b.build_float_compare(FloatPredicate::OLT, a.into_float_value(), b.into_float_value(), "").must(),
                self.b.build_float_compare(FloatPredicate::OGT, a.into_float_value(), b.into_float_value(), "").must(),
            ),
            TyKind::Str => {
                let [ap, al] = self.str_parts(a);
                let [bp, bl] = self.str_parts(b);
                return self.call_rt("lrt_str_cmp", &[ap, al, bp, bl]).unwrap().into_int_value();
            }
            TyKind::Sized(kind) if kind.is_float() => (
                self.b.build_float_compare(FloatPredicate::OLT, a.into_float_value(), b.into_float_value(), "").must(),
                self.b.build_float_compare(FloatPredicate::OGT, a.into_float_value(), b.into_float_value(), "").must(),
            ),
            TyKind::Sized(kind) => {
                let (lt, gt) = if kind.is_signed() {
                    (IntPredicate::SLT, IntPredicate::SGT)
                } else {
                    (IntPredicate::ULT, IntPredicate::UGT)
                };
                (
                    self.b.build_int_compare(lt, a.into_int_value(), b.into_int_value(), "").must(),
                    self.b.build_int_compare(gt, a.into_int_value(), b.into_int_value(), "").must(),
                )
            }
            TyKind::Struct(_) | TyKind::Enum(_) => {
                let ord = self.program.builtins.ord;
                if let Some(function) = self.explicit_method(ord, ty, 0) {
                    return self.call_method(function, &[a, b]).unwrap().into_int_value();
                }
                return self.call_cmp_helper(ty, a, b);
            }
            _ => return self.call_cmp_helper(ty, a, b),
        };
        self.sign(less, greater)
    }

    fn sign(&mut self, less: IntValue<'ctx>, greater: IntValue<'ctx>) -> IntValue<'ctx> {
        let upper = self.b.build_select(greater, self.i64(1), self.i64(0), "").must().into_int_value();
        self.b.build_select(less, self.i64(-1), upper, "").must().into_int_value()
    }

    fn call_cmp_helper(&mut self, ty: Ty, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let helper = self.helper(Helper::Cmp, ty);
        let call = self.b.build_call(helper, &[a.into(), b.into()], "").must();
        call.set_call_convention(crate::CALLCONV_FAST);
        call.try_as_basic_value().basic().unwrap().into_int_value()
    }

    /// Return `cmp` from the helper being generated if it is not zero; otherwise continue.
    fn return_if_unequal(&mut self, cmp: IntValue<'ctx>) {
        let unequal = self.b.build_int_compare(IntPredicate::NE, cmp, self.i64(0), "").must();
        let (out, next) = (self.block("differ"), self.block("same"));
        self.b.build_conditional_branch(unequal, out, next).must();
        self.b.position_at_end(out);
        self.b.build_return(Some(&cmp)).must();
        self.b.position_at_end(next);
    }

    /// Lexicographic comparison: tuples and lists element by element, structs field by field,
    /// enums by variant order and then fields (a derived `Ord`).
    fn cmp_body(&mut self, function: FunctionValue<'ctx>, ty: Ty) {
        let a = function.get_nth_param(0).unwrap();
        let b = function.get_nth_param(1).unwrap();
        match self.kind(ty).clone() {
            TyKind::Tuple(items) => {
                for (index, item) in items.iter().enumerate() {
                    let x = self.b.build_extract_value(a.into_struct_value(), index as u32, "").must();
                    let y = self.b.build_extract_value(b.into_struct_value(), index as u32, "").must();
                    let cmp = self.emit_cmp(*item, x, y);
                    self.return_if_unequal(cmp);
                }
            }
            TyKind::List(elem) => {
                let (la, lb) = (a.into_pointer_value(), b.into_pointer_value());
                let len_a = self.list_len(la);
                let len_b = self.list_len(lb);
                let shorter = self.b.build_int_compare(IntPredicate::SLT, len_a, len_b, "").must();
                let common = self.b.build_select(shorter, len_a, len_b, "").must().into_int_value();
                let counter = self.alloca(self.cx.i64_type().into(), "i");
                self.b.build_store(counter, self.i64(0)).must();
                let (header, body, exit) = (self.block("loop"), self.block("body"), self.block("end"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let i = self.b.build_load(self.cx.i64_type(), counter, "").must().into_int_value();
                let more = self.b.build_int_compare(IntPredicate::SLT, i, common, "").must();
                self.b.build_conditional_branch(more, body, exit).must();
                self.b.position_at_end(body);
                let x = self.list_elem_unchecked(la, i, elem);
                let y = self.list_elem_unchecked(lb, i, elem);
                let cmp = self.emit_cmp(elem, x, y);
                self.return_if_unequal(cmp);
                let next = self.b.build_int_add(i, self.i64(1), "").must();
                self.b.build_store(counter, next).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                let less = self.b.build_int_compare(IntPredicate::SLT, len_a, len_b, "").must();
                let greater = self.b.build_int_compare(IntPredicate::SGT, len_a, len_b, "").must();
                let result = self.sign(less, greater);
                self.b.build_return(Some(&result)).must();
                return;
            }
            TyKind::Struct(id) => {
                for index in 0..self.program.structs[id.0].fields.len() {
                    let field_ty = self.program.structs[id.0].fields[index].ty;
                    let x = self.struct_field(ty, index, a);
                    let y = self.struct_field(ty, index, b);
                    let cmp = self.emit_cmp(field_ty, x, y);
                    self.return_if_unequal(cmp);
                }
            }
            TyKind::Enum(id) => {
                let tag_a = self.enum_tag(ty, a);
                let tag_b = self.enum_tag(ty, b);
                let less = self.b.build_int_compare(IntPredicate::SLT, tag_a, tag_b, "").must();
                let greater = self.b.build_int_compare(IntPredicate::SGT, tag_a, tag_b, "").must();
                let by_tag = self.sign(less, greater);
                self.return_if_unequal(by_tag);
                let done = self.block("done");
                self.for_each_variant(ty, a, done, |this, variant| {
                    for field in 0..this.program.enums[id.0].variants[variant].fields.len() {
                        let field_ty = this.program.enums[id.0].variants[variant].fields[field].ty;
                        let x = this.variant_field(ty, variant, field, a);
                        let y = this.variant_field(ty, variant, field, b);
                        let cmp = this.emit_cmp(field_ty, x, y);
                        this.return_if_unequal(cmp);
                    }
                });
                self.b.position_at_end(done);
            }
            TyKind::Option(inner) => {
                // `None` sorts first.
                let a_some = self.opt_is_some(ty, a);
                let b_some = self.opt_is_some(ty, b);
                let less = self.b.build_int_compare(IntPredicate::ULT, a_some, b_some, "").must();
                let greater = self.b.build_int_compare(IntPredicate::UGT, a_some, b_some, "").must();
                let by_presence = self.sign(less, greater);
                self.return_if_unequal(by_presence);
                let (both, done) = (self.block("both"), self.block("done"));
                self.b.build_conditional_branch(a_some, both, done).must();
                self.b.position_at_end(both);
                let x = self.opt_inner(ty, a);
                let y = self.opt_inner(ty, b);
                let cmp = self.emit_cmp(inner, x, y);
                self.b.build_return(Some(&cmp)).must();
                self.b.position_at_end(done);
            }
            other => unreachable!("cannot order {other:?}"),
        }
        let zero = self.i64(0);
        self.b.build_return(Some(&zero)).must();
    }

    fn push_body(&mut self, function: FunctionValue<'ctx>, ty: Ty, repr: bool) {
        let builder = function.get_nth_param(0).unwrap().into_pointer_value();
        let value = function.get_nth_param(1).unwrap();
        match self.kind(ty).clone() {
            TyKind::Option(inner) => {
                let present = self.opt_is_some(ty, value);
                let (some, none, done) = (self.block("some"), self.block("none"), self.block("done"));
                self.b.build_conditional_branch(present, some, none).must();
                self.b.position_at_end(some);
                let inner_value = self.opt_inner(ty, value);
                self.push_value(builder, inner, Some(inner_value), repr);
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(none);
                self.push_text(builder, "None");
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(done);
            }
            TyKind::Tuple(items) => {
                self.push_text(builder, "(");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.push_text(builder, ", ");
                    }
                    let element = self.b.build_extract_value(value.into_struct_value(), index as u32, "").must();
                    self.push_value(builder, *item, Some(element), true);
                }
                self.push_text(builder, if items.len() == 1 { ",)" } else { ")" });
            }
            TyKind::List(elem) => {
                let list = value.into_pointer_value();
                self.push_text(builder, "[");
                let counter = self.alloca(self.cx.i64_type().into(), "i");
                self.b.build_store(counter, self.i64(0)).must();
                let (header, body, separator, element, exit) = (
                    self.block("loop"),
                    self.block("body"),
                    self.block("separator"),
                    self.block("element"),
                    self.block("end"),
                );
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let i = self.b.build_load(self.cx.i64_type(), counter, "").must().into_int_value();
                let len = self.list_len(list);
                let more = self.b.build_int_compare(IntPredicate::SLT, i, len, "").must();
                self.b.build_conditional_branch(more, body, exit).must();
                self.b.position_at_end(body);
                let first = self.b.build_int_compare(IntPredicate::EQ, i, self.i64(0), "").must();
                self.b.build_conditional_branch(first, element, separator).must();
                self.b.position_at_end(separator);
                self.push_text(builder, ", ");
                self.b.build_unconditional_branch(element).must();
                self.b.position_at_end(element);
                let item = self.list_elem_unchecked(list, i, elem);
                self.push_value(builder, elem, Some(item), true);
                let next = self.b.build_int_add(i, self.i64(1), "").must();
                self.b.build_store(counter, next).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                self.push_text(builder, "]");
            }
            TyKind::Dict(key_ty, value_ty) => {
                let dict = value.into_pointer_value();
                self.push_text(builder, "{");
                let cursor = self.alloca(self.cx.i64_type().into(), "cursor");
                self.b.build_store(cursor, self.i64(0)).must();
                let first = self.alloca(self.cx.bool_type().into(), "first");
                self.b.build_store(first, self.bool(true)).must();
                let (header, body, separator, element, exit) = (
                    self.block("loop"),
                    self.block("body"),
                    self.block("separator"),
                    self.block("element"),
                    self.block("end"),
                );
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let entry = self.call_rt("lrt_dict_next", &[dict.into(), cursor.into()]).unwrap().into_pointer_value();
                let done = self.b.build_is_null(entry, "").must();
                self.b.build_conditional_branch(done, exit, body).must();
                self.b.position_at_end(body);
                let is_first = self.b.build_load(self.cx.bool_type(), first, "").must().into_int_value();
                self.b.build_store(first, self.bool(false)).must();
                self.b.build_conditional_branch(is_first, element, separator).must();
                self.b.position_at_end(separator);
                self.push_text(builder, ", ");
                self.b.build_unconditional_branch(element).must();
                self.b.position_at_end(element);
                let key = self.entry_key(entry, key_ty);
                self.push_value(builder, key_ty, Some(key), true);
                self.push_text(builder, ": ");
                let item = self.entry_value(entry, key_ty, value_ty);
                self.push_value(builder, value_ty, Some(item), true);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
                self.push_text(builder, "}");
            }
            TyKind::Struct(id) => {
                // Like a Python dataclass: `Point(x=1, y=2)`.
                let def = &self.program.structs[id.0];
                let (name, fields): (String, Vec<(String, Ty)>) =
                    (def.name.clone(), def.fields.iter().map(|f| (f.name.clone(), f.ty)).collect());
                self.push_text(builder, &format!("{name}("));
                for (index, (field_name, field_ty)) in fields.iter().enumerate() {
                    let separator = if index == 0 { "" } else { ", " };
                    self.push_text(builder, &format!("{separator}{field_name}="));
                    let field = self.struct_field(ty, index, value);
                    self.push_value(builder, *field_ty, Some(field), true);
                }
                self.push_text(builder, ")");
            }
            TyKind::Enum(id) => {
                let done = self.block("printed");
                self.for_each_variant(ty, value, done, |this, variant| {
                    let def = &this.program.enums[id.0].variants[variant];
                    let (name, fields): (String, Vec<(String, Ty)>) =
                        (def.name.clone(), def.fields.iter().map(|f| (f.name.clone(), f.ty)).collect());
                    this.push_text(builder, &name);
                    if fields.is_empty() {
                        return;
                    }
                    this.push_text(builder, "(");
                    for (index, (field_name, field_ty)) in fields.iter().enumerate() {
                        let separator = if index == 0 { "" } else { ", " };
                        this.push_text(builder, &format!("{separator}{field_name}="));
                        let field = this.variant_field(ty, variant, index, value);
                        this.push_value(builder, *field_ty, Some(field), true);
                    }
                    this.push_text(builder, ")");
                });
                self.b.position_at_end(done);
            }
            TyKind::Result(ok_ty, err_ty) => {
                let ok = self.result_is_ok(value);
                let (ok_block, err_block, done) = (self.block("ok"), self.block("err"), self.block("done"));
                self.b.build_conditional_branch(ok, ok_block, err_block).must();
                self.b.position_at_end(ok_block);
                self.push_text(builder, "Ok(");
                let payload = self.result_ok(ty, value);
                self.push_value(builder, ok_ty, payload, true);
                self.push_text(builder, ")");
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(err_block);
                self.push_text(builder, "Err(");
                let error = self.result_err(ty, value);
                self.push_value(builder, err_ty, Some(error), true);
                self.push_text(builder, ")");
                self.b.build_unconditional_branch(done).must();
                self.b.position_at_end(done);
            }
            other => unreachable!("no print helper for {other:?}"),
        }
        self.b.build_return(None).must();
    }
}
