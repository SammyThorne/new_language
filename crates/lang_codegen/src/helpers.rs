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
            other => unreachable!("no equality helper for {other:?}"),
        };
        self.b.build_return(Some(&result)).must();
    }

    fn emit_hash(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        match self.kind(ty).clone() {
            TyKind::Int => value.into_int_value(),
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

    fn emit_cmp(&mut self, ty: Ty, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
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
            other => unreachable!("cannot order {other:?}"),
        };
        let upper = self.b.build_select(greater, self.i64(1), self.i64(0), "").must().into_int_value();
        self.b.build_select(less, self.i64(-1), upper, "").must().into_int_value()
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
            other => unreachable!("no print helper for {other:?}"),
        }
        self.b.build_return(None).must();
    }
}
