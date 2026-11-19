//! Enums, `Result`, trait objects and calls to trait implementations.

use crate::{CALLCONV_FAST, Codegen, Helper, Must};
use inkwell::IntPredicate;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use lang_sema::hir::{Conversion, Expr};
use lang_sema::types::{TraitId, Ty, TyKind};

impl<'ctx> Codegen<'ctx, '_> {
    // ---- enums --------------------------------------------------------------------------

    fn enum_index(&self, ty: Ty) -> usize {
        match self.kind(ty) {
            TyKind::Enum(id) => id.0,
            other => unreachable!("not an enum: {other:?}"),
        }
    }

    /// A value of a user enum. Variants without fields are static objects; others are allocated.
    pub(crate) fn variant(&mut self, ty: Ty, index: usize, fields: &[Expr]) -> BasicValueEnum<'ctx> {
        let enum_index = self.enum_index(ty);
        let variant_type = self.variant_types[enum_index][index];
        let desc = self.desc(ty);
        if fields.is_empty() {
            if let Some(&object) = self.unit_variants.get(&(enum_index, index)) {
                return object.into();
            }
            let init = variant_type.const_named_struct(&[desc.into(), self.i64(index as i64).into()]);
            let def = &self.program.enums[enum_index];
            let global = self.module.add_global(
                variant_type,
                None,
                &format!("variant.{}.{}", def.name, def.variants[index].name),
            );
            global.set_initializer(&init);
            global.set_constant(true);
            global.set_linkage(Linkage::Private);
            let object = global.as_pointer_value();
            self.unit_variants.insert((enum_index, index), object);
            return object.into();
        }
        let values: Vec<BasicValueEnum> = fields.iter().map(|f| self.value(f)).collect();
        self.variant_values(ty, index, values)
    }

    /// A value of variant `index` with these field values.
    pub(crate) fn variant_values(
        &mut self,
        ty: Ty,
        index: usize,
        values: Vec<BasicValueEnum<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        if values.is_empty() {
            return self.variant(ty, index, &[]);
        }
        let enum_index = self.enum_index(ty);
        let variant_type = self.variant_types[enum_index][index];
        let desc = self.desc(ty);
        let object = self.alloc_object(variant_type, desc);
        let tag = self.b.build_struct_gep(variant_type, object, 1, "").must();
        self.b.build_store(tag, self.i64(index as i64)).must();
        for (i, value) in values.into_iter().enumerate() {
            let ptr = self.b.build_struct_gep(variant_type, object, i as u32 + 2, "").must();
            self.b.build_store(ptr, value).must();
        }
        object.into()
    }

    pub(crate) fn enum_tag(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let enum_index = self.enum_index(ty);
        let header = self.variant_types[enum_index][0];
        let ptr = self.b.build_struct_gep(header, value.into_pointer_value(), 1, "").must();
        self.b.build_load(self.cx.i64_type(), ptr, "tag").must().into_int_value()
    }

    /// Field `field` of a value known to be variant `index`.
    pub(crate) fn variant_field(
        &mut self,
        ty: Ty,
        index: usize,
        field: usize,
        value: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let enum_index = self.enum_index(ty);
        let variant_type = self.variant_types[enum_index][index];
        let field_ty = self.program.enums[enum_index].variants[index].fields[field].ty;
        let ptr = self.b.build_struct_gep(variant_type, value.into_pointer_value(), field as u32 + 2, "").must();
        self.b.build_load(self.value_type(field_ty), ptr, "").must()
    }

    // ---- Result -------------------------------------------------------------------------

    fn result_parts(&self, ty: Ty) -> (Ty, Ty) {
        match self.kind(ty) {
            TyKind::Result(ok, err) => (*ok, *err),
            other => unreachable!("not a Result: {other:?}"),
        }
    }

    fn result_err_index(&self, ty: Ty) -> u32 {
        if self.result_parts(ty).0 == Ty::NONE { 1 } else { 2 }
    }

    pub(crate) fn make_result(
        &mut self,
        ty: Ty,
        ok: bool,
        payload: Option<BasicValueEnum<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        let struct_type = self.value_type(ty).into_struct_type();
        let mut value = self.b.build_insert_value(struct_type.const_zero(), self.bool(ok), 0, "").must();
        if let Some(payload) = payload {
            let index = if ok { 1 } else { self.result_err_index(ty) };
            value = self.b.build_insert_value(value, payload, index, "").must();
        }
        value.into_struct_value().into()
    }

    pub(crate) fn result_is_ok(&mut self, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        self.b.build_extract_value(value.into_struct_value(), 0, "ok").must().into_int_value()
    }

    pub(crate) fn result_ok(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> Option<BasicValueEnum<'ctx>> {
        if self.result_parts(ty).0 == Ty::NONE {
            return None;
        }
        Some(self.b.build_extract_value(value.into_struct_value(), 1, "").must())
    }

    pub(crate) fn result_err(&mut self, ty: Ty, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let index = self.result_err_index(ty);
        self.b.build_extract_value(value.into_struct_value(), index, "").must()
    }

    /// `value?`: continue with the payload, or return the error (converted) or `None`.
    pub(crate) fn try_value(&mut self, inner: &Expr, conversion: &Conversion) -> Option<BasicValueEnum<'ctx>> {
        let value = self.value(inner);
        let ret = self.state.as_ref().expect("inside a function").ret;
        let (present, fail_value) = match self.kind(inner.ty).clone() {
            TyKind::Result(_, err) => (self.result_is_ok(value), Some(err)),
            TyKind::Option(_) => (self.opt_is_some(inner.ty, value), None),
            other => unreachable!("`?` on {other:?}"),
        };
        let fail = self.block("try_fail");
        let cont = self.block("try_ok");
        self.b.build_conditional_branch(present, cont, fail).must();
        self.b.position_at_end(fail);
        let returned = match fail_value {
            Some(err_ty) => {
                let error = self.result_err(inner.ty, value);
                let (_, ret_err) = self.result_parts(ret);
                let converted = self.convert(error, conversion, err_ty, ret_err);
                self.make_result(ret, false, Some(converted))
            }
            None => self.none_value(ret),
        };
        self.run_cleanups(0);
        self.b.build_return(Some(&returned)).must();
        self.b.position_at_end(cont);
        match self.kind(inner.ty) {
            TyKind::Result(..) => self.result_ok(inner.ty, value),
            _ => Some(self.opt_inner(inner.ty, value)),
        }
    }

    // ---- trait objects ------------------------------------------------------------------

    pub(crate) fn make_dyn(&mut self, data: PointerValue<'ctx>, vtable: PointerValue<'ctx>) -> BasicValueEnum<'ctx> {
        let with_data = self.b.build_insert_value(self.dyn_type.get_undef(), data, 0, "").must();
        self.b.build_insert_value(with_data, vtable, 1, "").must().into_struct_value().into()
    }

    /// Apply a conversion to a value of type `from`, producing one of type `to`.
    pub(crate) fn convert(
        &mut self,
        value: BasicValueEnum<'ctx>,
        conversion: &Conversion,
        from: Ty,
        _to: Ty,
    ) -> BasicValueEnum<'ctx> {
        match conversion {
            Conversion::Identity => value,
            Conversion::ToDyn(trait_id) => {
                let vtable = self.vtable(from, *trait_id);
                self.make_dyn(value.into_pointer_value(), vtable)
            }
            Conversion::Upcast(path) => self.upcast(value, path),
        }
    }

    pub(crate) fn upcast(&mut self, value: BasicValueEnum<'ctx>, path: &[usize]) -> BasicValueEnum<'ctx> {
        let object = value.into_struct_value();
        let data = self.b.build_extract_value(object, 0, "").must().into_pointer_value();
        let mut vtable = self.b.build_extract_value(object, 1, "").must().into_pointer_value();
        for &slot in path {
            vtable = self.vtable_slot(vtable, 1 + slot);
        }
        self.make_dyn(data, vtable)
    }

    fn vtable_slot(&mut self, vtable: PointerValue<'ctx>, slot: usize) -> PointerValue<'ctx> {
        let index = self.i64(slot as i64);
        // SAFETY (LLVM): the slot is within the vtable built for this trait.
        let ptr = unsafe { self.b.build_in_bounds_gep(self.ptr_type(), vtable, &[index], "").must() };
        self.b.build_load(self.ptr_type(), ptr, "").must().into_pointer_value()
    }

    /// The vtable of a struct or enum for a trait, built on first use.
    pub(crate) fn vtable(&mut self, ty: Ty, trait_id: TraitId) -> PointerValue<'ctx> {
        if let Some(&vtable) = self.vtables.get(&(ty, trait_id)) {
            return vtable;
        }
        let def = &self.program.traits[trait_id.0];
        let implementation = self.program.impl_of(trait_id, ty).expect("the type checker verified the implementation");
        let methods = implementation.methods.clone();
        let mut entries: Vec<PointerValue> = vec![self.desc(ty)];
        for &required in &def.supertraits {
            entries.push(self.vtable(ty, required));
        }
        for (index, method) in methods.iter().enumerate() {
            let function = match method {
                Some(func) => self.function_value(*func),
                None => self.derived_method(trait_id, index, ty),
            };
            entries.push(function.as_global_value().as_pointer_value());
        }
        let array = self.ptr_type().const_array(&entries);
        let name = format!("vtable.{}.{}", self.program.types.display(ty), def.name);
        let global = self.module.add_global(array.get_type(), None, &name);
        global.set_initializer(&array);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        let ptr = global.as_pointer_value();
        self.vtables.insert((ty, trait_id), ptr);
        ptr
    }

    /// A function for a derived trait method, so it can sit in a vtable. Only `Display` and
    /// `Hash` can be both derived and used as trait objects.
    fn derived_method(&mut self, trait_id: TraitId, index: usize, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&f) = self.derived_methods.get(&(trait_id, ty)) {
            return f;
        }
        let b = self.program.builtins;
        let ret: inkwell::types::BasicTypeEnum =
            if trait_id == b.display { self.str_type.into() } else { self.cx.i64_type().into() };
        let fn_type = ret.fn_type(&[self.ptr_type().into()], false);
        let name = format!(
            "derived.{}.{}",
            self.program.types.display(ty),
            self.program.traits[trait_id.0].methods[index].name
        );
        let function = self.module.add_function(&name, fn_type, Some(Linkage::Private));
        function.set_call_conventions(CALLCONV_FAST);
        self.derived_methods.insert((trait_id, ty), function);
        self.in_function(function, |this| {
            let value: BasicValueEnum = function.get_nth_param(0).unwrap();
            let result = if trait_id == b.display {
                this.display_string(ty, Some(value))
            } else {
                this.emit_hash(ty, value).into()
            };
            this.b.build_return(Some(&result)).must();
        });
        function
    }

    /// Call method `method` of `trait_id` through a trait object whose trait reaches `trait_id`
    /// by `path`. The call goes through the trait method's dispatcher (`dispatcher`).
    pub(crate) fn dyn_call(
        &mut self,
        receiver: BasicValueEnum<'ctx>,
        trait_id: TraitId,
        path: &[usize],
        method: usize,
        args: Vec<BasicValueEnum<'ctx>>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let target = self.upcast(receiver, path).into_struct_value();
        let data = self.b.build_extract_value(target, 0, "").must();
        let vtable = self.b.build_extract_value(target, 1, "").must();
        let dispatcher = self.dispatcher(trait_id, method);
        let mut values: Vec<BasicMetadataValueEnum> = vec![vtable.into(), data.into()];
        values.extend(args.into_iter().map(BasicMetadataValueEnum::from));
        let call = self.b.build_call(dispatcher, &values, "").must();
        call.set_call_convention(CALLCONV_FAST);
        call.try_as_basic_value().basic()
    }

    /// The type of method `method` of `trait_id` as called through a vtable: the object's data
    /// pointer, then the arguments.
    fn method_type(&self, trait_id: TraitId, method: usize, leading: usize) -> inkwell::types::FunctionType<'ctx> {
        let signature = &self.program.traits[trait_id.0].methods[method];
        let mut params: Vec<BasicMetadataTypeEnum> = vec![self.ptr_type().into(); leading];
        params.extend(signature.params.iter().map(|t| BasicMetadataTypeEnum::from(self.value_type(*t))));
        match self.llty(signature.ret) {
            Some(ret) => ret.fn_type(&params, false),
            None => self.cx.void_type().fn_type(&params, false),
        }
    }

    /// `dispatch.Trait.method(vtable, data, args...)`, always inlined. Declared here; its body
    /// is written by `define_dispatchers` once the whole program has been generated, when every
    /// vtable of the trait is known.
    fn dispatcher(&mut self, trait_id: TraitId, method: usize) -> FunctionValue<'ctx> {
        if let Some(&f) = self.dispatchers.get(&(trait_id, method)) {
            return f;
        }
        let def = &self.program.traits[trait_id.0];
        let name = format!("dispatch.{}.{}", def.name, def.methods[method].name);
        let function = self.module.add_function(&name, self.method_type(trait_id, method, 2), Some(Linkage::Private));
        function.set_call_conventions(CALLCONV_FAST);
        let always = self.cx.create_enum_attribute(Attribute::get_named_enum_kind_id("alwaysinline"), 0);
        function.add_attribute(AttributeLoc::Function, always);
        self.dispatchers.insert((trait_id, method), function);
        function
    }

    /// Write the dispatchers' bodies (docs/09, phase 4, devirtualisation). The compiler sees
    /// the whole program, so it knows every type converted to each trait. When there are at
    /// most `DIRECT_CALLS` of them, a dispatcher compares the vtable with each one's and calls
    /// that type's method directly, which LLVM can then inline; the indirect call through the
    /// vtable stays as the last case. With more, the dispatcher is just the indirect call.
    pub(crate) fn define_dispatchers(&mut self) {
        const DIRECT_CALLS: usize = 4;
        // Writing a body can generate a derived method that itself calls through a trait
        // object, so repeat until every dispatcher has a body. A vtable made after a body was
        // written is still handled, by the indirect call.
        loop {
            let mut keys: Vec<(TraitId, usize)> =
                self.dispatchers.iter().filter(|(_, f)| f.count_basic_blocks() == 0).map(|(k, _)| *k).collect();
            if keys.is_empty() {
                return;
            }
            keys.sort_by_key(|(t, m)| (t.0, *m));
            for (trait_id, method) in keys {
                self.define_dispatcher(trait_id, method, DIRECT_CALLS);
            }
        }
    }

    fn define_dispatcher(&mut self, trait_id: TraitId, method: usize, direct_calls: usize) {
        {
            let function = self.dispatchers[&(trait_id, method)];
            // Every vtable for the trait, in a fixed order.
            let mut cases: Vec<(Ty, PointerValue<'ctx>)> =
                self.vtables.iter().filter(|((_, t), _)| *t == trait_id).map(|((ty, _), v)| (*ty, *v)).collect();
            cases.sort_by_key(|(_, vtable)| vtable.get_name().to_string_lossy().into_owned());
            if cases.len() > direct_calls {
                cases.clear();
            }
            let targets: Vec<(PointerValue<'ctx>, FunctionValue<'ctx>)> = cases
                .iter()
                .map(|&(ty, vtable)| {
                    let target = match self.explicit_method(trait_id, ty, method) {
                        Some(f) => f,
                        None => self.derived_method(trait_id, method, ty),
                    };
                    (vtable, target)
                })
                .collect();
            let slot = 1 + self.program.traits[trait_id.0].supertraits.len() + method;
            let fn_type = self.method_type(trait_id, method, 1);
            self.in_function(function, |this| {
                let vtable = function.get_nth_param(0).unwrap().into_pointer_value();
                let args: Vec<BasicMetadataValueEnum> =
                    function.get_param_iter().skip(1).map(BasicMetadataValueEnum::from).collect();
                let finish = |this: &mut Self, result: Option<BasicValueEnum<'ctx>>| match result {
                    Some(value) => this.b.build_return(Some(&value)).must(),
                    None => this.b.build_return(None).must(),
                };
                for (expected, target) in targets {
                    let direct = this.cx.append_basic_block(function, "direct");
                    let next = this.cx.append_basic_block(function, "next");
                    let same = this.b.build_int_compare(IntPredicate::EQ, vtable, expected, "").must();
                    this.b.build_conditional_branch(same, direct, next).must();
                    this.b.position_at_end(direct);
                    let call = this.b.build_call(target, &args, "").must();
                    call.set_call_convention(CALLCONV_FAST);
                    finish(this, call.try_as_basic_value().basic());
                    this.b.position_at_end(next);
                }
                let pointer = this.vtable_slot(vtable, slot);
                let call = this.b.build_indirect_call(fn_type, pointer, &args, "").must();
                call.set_call_convention(CALLCONV_FAST);
                finish(this, call.try_as_basic_value().basic());
            });
        }
    }

    // ---- implementations of the built-in traits -----------------------------------------

    /// The function implementing method `index` of `trait_id` for `ty`, if written by hand.
    pub(crate) fn explicit_method(&self, trait_id: TraitId, ty: Ty, index: usize) -> Option<FunctionValue<'ctx>> {
        let implementation = self.program.impl_of(trait_id, ty)?;
        implementation.methods.get(index).copied().flatten().map(|f| self.function_value(f))
    }

    pub(crate) fn call_method(
        &mut self,
        function: FunctionValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let args: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();
        let call = self.b.build_call(function, &args, "").must();
        call.set_call_convention(CALLCONV_FAST);
        call.try_as_basic_value().basic()
    }

    /// Whether a trait object of `trait_id` can reach `target`, and how.
    pub(crate) fn dyn_path(&self, from: TraitId, target: TraitId) -> Option<Vec<usize>> {
        if from == target {
            return Some(Vec::new());
        }
        for (slot, &s) in self.program.traits[from.0].supertraits.iter().enumerate() {
            if let Some(mut rest) = self.dyn_path(s, target) {
                rest.insert(0, slot);
                return Some(rest);
            }
        }
        None
    }

    /// The text of a value for messages: `Display` when it has it, else `Error.message`.
    pub(crate) fn push_user_value(
        &mut self,
        builder: PointerValue<'ctx>,
        ty: Ty,
        value: BasicValueEnum<'ctx>,
        repr: bool,
    ) {
        let b = self.program.builtins;
        match *self.kind(ty) {
            TyKind::Dyn(t) | TyKind::SelfType(t) => {
                let (trait_id, path) = match self.dyn_path(t, b.display) {
                    Some(path) => (b.display, path),
                    None => (
                        b.error,
                        self.dyn_path(t, b.error).expect("printable trait objects implement Display or Error"),
                    ),
                };
                let text = self.dyn_call(value, trait_id, &path, 0, Vec::new()).unwrap();
                let [ptr, len] = self.str_parts(text);
                self.builder_push(builder, ptr, len);
            }
            _ => {
                if let Some(function) = self.explicit_method(b.display, ty, 0) {
                    let text = self.call_method(function, &[value]).unwrap();
                    let [ptr, len] = self.str_parts(text);
                    self.builder_push(builder, ptr, len);
                } else if self.program.impl_of(b.display, ty).is_some() {
                    let helper = self.helper(Helper::Push { repr }, ty);
                    let call = self.b.build_call(helper, &[builder.into(), value.into()], "").must();
                    call.set_call_convention(CALLCONV_FAST);
                } else if let Some(function) = self.explicit_method(b.error, ty, 0) {
                    let text = self.call_method(function, &[value]).unwrap();
                    let [ptr, len] = self.str_parts(text);
                    self.builder_push(builder, ptr, len);
                } else {
                    let name = self.program.types.display(ty);
                    self.push_text(builder, &name);
                }
            }
        }
    }

    /// The text of an error for a panic message: `Error.message` when the type implements
    /// `Error`, otherwise how it prints.
    pub(crate) fn push_error_text(&mut self, builder: PointerValue<'ctx>, ty: Ty, value: BasicValueEnum<'ctx>) {
        let error = self.program.builtins.error;
        match *self.kind(ty) {
            TyKind::Dyn(t) | TyKind::SelfType(t) => {
                if let Some(path) = self.dyn_path(t, error) {
                    let text = self.dyn_call(value, error, &path, 0, Vec::new()).unwrap();
                    let [ptr, len] = self.str_parts(text);
                    self.builder_push(builder, ptr, len);
                    return;
                }
            }
            TyKind::Struct(_) | TyKind::Enum(_) => {
                if let Some(function) = self.explicit_method(error, ty, 0) {
                    let text = self.call_method(function, &[value]).unwrap();
                    let [ptr, len] = self.str_parts(text);
                    self.builder_push(builder, ptr, len);
                    return;
                }
            }
            _ => {}
        }
        self.push_value(builder, ty, Some(value), false);
    }

    /// The `ParseError` from the prelude, for a failed `int.parse` or `float.parse`.
    /// A new struct object with these field values.
    pub(crate) fn new_struct(&mut self, ty: Ty, values: &[BasicValueEnum<'ctx>]) -> PointerValue<'ctx> {
        let TyKind::Struct(id) = *self.kind(ty) else { unreachable!("not a struct") };
        let struct_type = self.struct_types[id.0];
        let desc = self.desc(ty);
        let object = self.alloc_object(struct_type, desc);
        for (index, value) in values.iter().enumerate() {
            let ptr = self.b.build_struct_gep(struct_type, object, index as u32 + 1, "").must();
            self.b.build_store(ptr, *value).must();
        }
        object
    }

    pub(crate) fn parse_error(&mut self, text: BasicValueEnum<'ctx>, expected: &str) -> BasicValueEnum<'ctx> {
        let id = self.program.parse_error;
        let struct_type = self.struct_types[id.0];
        let ty = self.program.structs[id.0].ty;
        let desc = self.desc(ty);
        let object = self.alloc_object(struct_type, desc);
        let expected = self.str_const(expected);
        for (index, value) in [text, expected.as_basic_value_enum()].into_iter().enumerate() {
            let ptr = self.b.build_struct_gep(struct_type, object, index as u32 + 1, "").must();
            self.b.build_store(ptr, value).must();
        }
        object.into()
    }
}
