//! Expressions, operators, and the representations of lists, dicts, tuples and optionals.

use crate::{Codegen, Helper, Must};
use inkwell::types::BasicType;
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};
use lang_diag::Span;
use lang_sema::hir::{BinaryOp, Expr, ExprKind, FPart, UnaryOp};
use lang_sema::types::{Ty, TyKind};

impl<'ctx> Codegen<'ctx, '_> {
    pub(crate) fn value(&mut self, expr: &Expr) -> BasicValueEnum<'ctx> {
        self.expr(expr).expect("expression has a value")
    }

    pub(crate) fn expr(&mut self, expr: &Expr) -> Option<BasicValueEnum<'ctx>> {
        self.set_location(expr.span);
        Some(match &expr.kind {
            ExprKind::Int(value) => match self.kind(expr.ty) {
                TyKind::Sized(kind) => self.int_type(kind.bits()).const_int(*value as u64, false).into(),
                _ => self.i64(*value).into(),
            },
            ExprKind::Float(value) => match self.kind(expr.ty) {
                TyKind::Sized(_) => self.cx.f32_type().const_float(*value).into(),
                _ => self.cx.f64_type().const_float(*value).into(),
            },
            ExprKind::Bool(value) => self.bool(*value).into(),
            ExprKind::Str(text) => self.str_const(text).into(),
            ExprKind::None => {
                if expr.ty == Ty::NONE {
                    return None;
                }
                self.none_value(expr.ty)
            }
            ExprKind::Some(inner) => {
                let value = self.value(inner);
                self.wrap_some(expr.ty, value)
            }
            ExprKind::Local(local) => return self.load_local(*local, expr.ty),
            ExprKind::Call { func, args } => {
                let mut values: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.value(arg).into());
                }
                let call = self.b.build_call(self.function_value(*func), &values, "").must();
                if *func != self.program.entry {
                    call.set_call_convention(crate::CALLCONV_FAST);
                }
                return call.try_as_basic_value().basic();
            }
            ExprKind::Builtin { op, args } => return self.builtin(*op, args, expr),
            ExprKind::Unary { op, operand } => {
                let value = self.value(operand);
                match op {
                    UnaryOp::IntNeg => self.b.build_int_neg(value.into_int_value(), "").must().into(),
                    UnaryOp::FloatNeg => self.b.build_float_neg(value.into_float_value(), "").must().into(),
                    UnaryOp::Not | UnaryOp::IntInvert => self.b.build_not(value.into_int_value(), "").must().into(),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::And(lhs, rhs) | ExprKind::Or(lhs, rhs) => {
                let is_and = matches!(expr.kind, ExprKind::And(..));
                let left = self.value(lhs).into_int_value();
                let left_block = self.b.get_insert_block().unwrap();
                let right_block = self.block(if is_and { "and" } else { "or" });
                let merge = self.block("endlogic");
                if is_and {
                    self.b.build_conditional_branch(left, right_block, merge).must();
                } else {
                    self.b.build_conditional_branch(left, merge, right_block).must();
                }
                self.b.position_at_end(right_block);
                let right = self.value(rhs);
                let right_end = self.b.get_insert_block().unwrap();
                self.b.build_unconditional_branch(merge).must();
                self.b.position_at_end(merge);
                let phi = self.b.build_phi(self.cx.bool_type(), "").must();
                let short = self.bool(!is_and);
                phi.add_incoming(&[(&short as &dyn BasicValue, left_block), (&right, right_end)]);
                phi.as_basic_value()
            }
            ExprKind::IfElse { cond, then, otherwise } => {
                let cond = self.value(cond).into_int_value();
                return self.choose(cond, expr.ty, |this| this.expr(then), |this| this.expr(otherwise));
            }
            ExprKind::StructNew { id, fields } => {
                let struct_type = self.struct_types[id.0];
                let desc = self.desc(expr.ty);
                let values: Vec<BasicValueEnum> = fields.iter().map(|f| self.value(f)).collect();
                let object = if self.stack_sites.contains(&(expr as *const Expr)) {
                    self.stack_object(struct_type, desc)
                } else {
                    self.alloc_object(struct_type, desc)
                };
                for (index, value) in values.into_iter().enumerate() {
                    let ptr = self.b.build_struct_gep(struct_type, object, index as u32 + 1, "").must();
                    let tag = self.field_tag(expr.ty, index);
                    self.store_tagged(ptr, value, &tag);
                }
                object.into()
            }
            ExprKind::Field { object, index } => {
                let tag = self.field_tag(object.ty, *index);
                let ptr = self.field_ptr(object, *index);
                self.load_tagged(self.value_type(expr.ty), ptr, &tag, "")
            }
            ExprKind::List(items) => {
                let desc = self.desc(expr.ty);
                let list = self.call_rt("lrt_list_new", &[desc.into(), self.i64(items.len() as i64).into()]).unwrap();
                for item in items {
                    let value = self.value(item);
                    self.list_push(list.into_pointer_value(), value);
                }
                list
            }
            ExprKind::Dict(pairs) => {
                let desc = self.desc(expr.ty);
                let dict = self.call_rt("lrt_dict_new", &[desc.into()]).unwrap().into_pointer_value();
                for (key, value) in pairs {
                    let key = self.value(key);
                    let value = self.value(value);
                    let key_ptr = self.spill(key);
                    let slot =
                        self.call_rt("lrt_dict_slot", &[dict.into(), key_ptr.into()]).unwrap().into_pointer_value();
                    self.b.build_store(slot, value).must();
                }
                dict.into()
            }
            ExprKind::Tuple(items) => {
                let values: Vec<BasicValueEnum> = items.iter().map(|i| self.value(i)).collect();
                self.make_tuple(&values)
            }
            ExprKind::TupleGet { tuple, index } => {
                let tuple = self.value(tuple).into_struct_value();
                self.b.build_extract_value(tuple, *index as u32, "").must()
            }
            ExprKind::Index { container, index } => {
                let container_value = self.value(container).into_pointer_value();
                let index_value = self.value(index);
                match *self.kind(container.ty) {
                    TyKind::List(elem) => {
                        let ptr = self.list_elem_ptr(container_value, index_value.into_int_value(), elem, expr.span);
                        self.load_tagged(self.value_type(elem), ptr, "list.elem", "")
                    }
                    TyKind::Dict(key, _) => {
                        let ptr = self.dict_lookup(container_value, index_value, key, expr.span);
                        self.b.build_load(self.value_type(expr.ty), ptr, "").must()
                    }
                    ref other => unreachable!("cannot index {other:?}"),
                }
            }
            ExprKind::Slice { container, start, stop } => {
                let value = self.value(container);
                let bound = |this: &mut Self, e: &Option<Box<Expr>>| match e {
                    Some(e) => (this.value(e), this.i64(1)),
                    None => (this.i64(0).into(), this.i64(0)),
                };
                let (start, has_start) = bound(self, start);
                let (stop, has_stop) = bound(self, stop);
                if container.ty == Ty::STR {
                    let [ptr, len] = self.str_parts(value);
                    let (loc, loc_len) = self.location(expr.span);
                    self.call_rt(
                        "lrt_str_slice",
                        &[ptr, len, start, stop, has_start.into(), has_stop.into(), loc.into(), loc_len.into()],
                    )
                    .unwrap()
                } else {
                    self.call_rt("lrt_list_slice", &[value, start, stop, has_start.into(), has_stop.into()]).unwrap()
                }
            }
            ExprKind::FString(parts) => {
                let builder = self.builder_with_capacity(self.fstring_estimate(parts));
                for part in parts {
                    match part {
                        FPart::Literal(text) => self.push_text(builder, text),
                        FPart::Value { expr, spec: None } => {
                            let value = self.expr(expr);
                            self.push_value(builder, expr.ty, value, false);
                        }
                        FPart::Value { expr, spec: Some(spec) } => {
                            let value = self.value(expr);
                            let spec = self.str_const(spec);
                            let [spec_ptr, spec_len] = self.str_parts(spec.into());
                            match expr.ty {
                                Ty::INT => self.call_rt(
                                    "lrt_builder_push_format_int",
                                    &[builder.into(), value, spec_ptr, spec_len],
                                ),
                                Ty::FLOAT => self.call_rt(
                                    "lrt_builder_push_format_float",
                                    &[builder.into(), value, spec_ptr, spec_len],
                                ),
                                _ => {
                                    let [ptr, len] = self.str_parts(value);
                                    self.call_rt(
                                        "lrt_builder_push_format_str",
                                        &[builder.into(), ptr, len, spec_ptr, spec_len],
                                    )
                                }
                            };
                        }
                    }
                }
                self.call_rt("lrt_builder_finish", &[builder.into()]).unwrap()
            }
            ExprKind::Variant { index, fields } => self.variant(expr.ty, *index, fields),
            ExprKind::Ok(payload) => {
                let payload = self.expr(payload);
                self.make_result(expr.ty, true, payload)
            }
            ExprKind::Err(error) => {
                let error = self.value(error);
                self.make_result(expr.ty, false, Some(error))
            }
            ExprKind::ToDyn { value, trait_id } => {
                let data = self.value(value).into_pointer_value();
                let vtable = self.vtable(value.ty, *trait_id);
                self.make_dyn(data, vtable)
            }
            ExprKind::Upcast { value, path } => {
                let object = self.value(value);
                self.upcast(object, path)
            }
            ExprKind::DynCall { receiver, trait_id, path, method, args } => {
                let receiver = self.value(receiver);
                let args: Vec<BasicValueEnum> = args.iter().map(|a| self.value(a)).collect();
                return self.dyn_call(receiver, *trait_id, path, *method, args);
            }
            ExprKind::Try { value, conversion } => return self.try_value(value, conversion),
            ExprKind::Closure { func, captures } => {
                let code = self.function_value(*func).as_global_value().as_pointer_value();
                let env = if captures.is_empty() {
                    self.ptr_type().const_null()
                } else {
                    let size = self.i64(8 * captures.len() as i64);
                    let env = self.call_rt("lrt_alloc_raw", &[size.into()]).unwrap().into_pointer_value();
                    for (index, local) in captures.iter().enumerate() {
                        let cell = self.local_cell(*local);
                        // SAFETY (LLVM): the environment was allocated with one pointer per capture.
                        let at = unsafe {
                            self.b.build_in_bounds_gep(self.ptr_type(), env, &[self.i64(index as i64)], "").must()
                        };
                        self.b.build_store(at, cell).must();
                    }
                    env
                };
                self.make_dyn(code, env)
            }
            ExprKind::FuncRef(func) => {
                let adapter = self.adapter(*func, false).as_global_value().as_pointer_value();
                self.make_dyn(adapter, self.ptr_type().const_null())
            }
            ExprKind::BoundMethod { receiver, func } => {
                let receiver = self.value(receiver).into_pointer_value();
                let adapter = self.adapter(*func, true).as_global_value().as_pointer_value();
                self.make_dyn(adapter, receiver)
            }
            ExprKind::CallValue { callee, args } => {
                let callee_value = self.value(callee);
                let values: Vec<BasicValueEnum> = args.iter().map(|a| self.value(a)).collect();
                return self.call_function_value(callee.ty, callee_value, &values);
            }
            ExprKind::Block { stmts, value } => {
                self.stmts(stmts);
                return self.expr(value);
            }
            ExprKind::Spawn { func, callee, args } => self.spawn(expr.ty, *func, callee.as_deref(), args),
        })
    }

    /// `spawn`: put the arguments (and the function value, if any) in an environment, and start
    /// a task running a generated entry function that unpacks them, makes the call, and stores
    /// the result in the task.
    fn spawn(
        &mut self,
        task_ty: Ty,
        func: Option<lang_sema::hir::FuncId>,
        callee: Option<&Expr>,
        args: &[Expr],
    ) -> BasicValueEnum<'ctx> {
        let TyKind::Task(result_ty) = *self.kind(task_ty) else { unreachable!("spawn gives a task") };
        let ptr = self.ptr_type();
        let callee_value = callee.map(|c| self.value(c));
        let arg_values: Vec<BasicValueEnum> = args.iter().map(|a| self.value(a)).collect();
        let result_size = self.llty(result_ty).map_or(0, |t| self.td.get_abi_size(&t));
        let task = self.call_rt("lrt_task_new", &[self.i64(result_size as i64).into()]).unwrap().into_pointer_value();

        let mut fields: Vec<inkwell::types::BasicTypeEnum> = vec![ptr.into()];
        if callee_value.is_some() {
            fields.push(self.dyn_type.into());
        }
        fields.extend(arg_values.iter().map(|v| v.get_type()));
        let env_type = self.cx.struct_type(&fields, false);
        let env_size = self.i64(self.td.get_abi_size(&env_type) as i64);
        let env = self.call_rt("lrt_alloc_raw", &[env_size.into()]).unwrap().into_pointer_value();
        let mut stored: Vec<BasicValueEnum> = vec![task.into()];
        stored.extend(callee_value);
        stored.extend(arg_values);
        for (index, value) in stored.iter().enumerate() {
            let at = self.b.build_struct_gep(env_type, env, index as u32, "").must();
            self.b.build_store(at, *value).must();
        }

        // The entry function, called by the runtime on the new thread with the C convention.
        let entry_type = self.cx.void_type().fn_type(&[ptr.into()], false);
        let entry = self.module.add_function("spawn.entry", entry_type, Some(inkwell::module::Linkage::Private));
        entry.add_attribute(inkwell::attributes::AttributeLoc::Function, self.unwind_table);
        let has_callee = callee.is_some();
        let callee_ty = callee.map(|c| c.ty);
        self.in_function(entry, |this| {
            let env = entry.get_nth_param(0).unwrap().into_pointer_value();
            let load = |this: &mut Self, index: usize| {
                let at = this.b.build_struct_gep(env_type, env, index as u32, "").must();
                this.b.build_load(fields[index], at, "").must()
            };
            let task = load(this, 0).into_pointer_value();
            let first_arg = if has_callee { 2 } else { 1 };
            let values: Vec<BasicValueEnum> = (first_arg..fields.len()).map(|i| load(this, i)).collect();
            let result = match func {
                Some(func) => {
                    let args: Vec<BasicMetadataValueEnum> = values.iter().map(|v| (*v).into()).collect();
                    let call = this.b.build_call(this.function_value(func), &args, "").must();
                    if func != this.program.entry {
                        call.set_call_convention(crate::CALLCONV_FAST);
                    }
                    call.try_as_basic_value().basic()
                }
                None => {
                    let pair = load(this, 1);
                    this.call_function_value(callee_ty.expect("a callee"), pair, &values)
                }
            };
            if let Some(result) = result {
                let slot = this.call_rt("lrt_task_result", &[task.into()]).unwrap().into_pointer_value();
                this.b.build_store(slot, result).must();
            }
            this.b.build_return(None).must();
        });
        let entry_ptr = entry.as_global_value().as_pointer_value();
        self.call_rt("lrt_spawn", &[entry_ptr.into(), env.into(), task.into()]);
        task.into()
    }

    /// The LLVM type of the code of a function value: environment first, then the arguments.
    fn closure_fn_type(&self, ty: Ty) -> inkwell::types::FunctionType<'ctx> {
        let TyKind::Func(params, ret) = self.kind(ty) else { unreachable!("not a function type") };
        let mut types: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![self.ptr_type().into()];
        types.extend(params.iter().map(|p| -> inkwell::types::BasicMetadataTypeEnum { self.value_type(*p).into() }));
        match self.llty(*ret) {
            Some(ret) => ret.fn_type(&types, false),
            None => self.cx.void_type().fn_type(&types, false),
        }
    }

    /// Call a function value of type `ty`.
    pub(crate) fn call_function_value(
        &mut self,
        ty: Ty,
        callee: BasicValueEnum<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let pair = callee.into_struct_value();
        let code = self.b.build_extract_value(pair, 0, "code").must().into_pointer_value();
        let env = self.b.build_extract_value(pair, 1, "env").must();
        let mut values: Vec<BasicMetadataValueEnum> = vec![env.into()];
        values.extend(args.iter().map(|a| -> BasicMetadataValueEnum { (*a).into() }));
        let fn_type = self.closure_fn_type(ty);
        let call = self.b.build_indirect_call(fn_type, code, &values, "").must();
        call.set_call_convention(crate::CALLCONV_FAST);
        call.try_as_basic_value().basic()
    }

    /// A function with the closure calling convention that calls `func`: ignoring the
    /// environment, or (`bound`) passing it as the receiver.
    fn adapter(&mut self, func: lang_sema::hir::FuncId, bound: bool) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(&adapter) = self.adapters.get(&(func, bound)) {
            return adapter;
        }
        let target = self.function_value(func);
        let function = &self.program.functions[func.0];
        let skip = usize::from(bound);
        let params: Vec<Ty> = function.params.iter().skip(skip).map(|l| function.locals[l.0].ty).collect();
        let mut types: Vec<inkwell::types::BasicMetadataTypeEnum> = vec![self.ptr_type().into()];
        types.extend(params.iter().map(|p| -> inkwell::types::BasicMetadataTypeEnum { self.value_type(*p).into() }));
        let fn_type = match self.llty(function.ret) {
            Some(ret) => ret.fn_type(&types, false),
            None => self.cx.void_type().fn_type(&types, false),
        };
        let name = format!("{}.adapter", function.name);
        let adapter = self.module.add_function(&name, fn_type, Some(inkwell::module::Linkage::Private));
        adapter.set_call_conventions(crate::CALLCONV_FAST);
        adapter.add_attribute(inkwell::attributes::AttributeLoc::Function, self.unwind_table);
        self.adapters.insert((func, bound), adapter);
        self.in_function(adapter, |this| {
            let mut args: Vec<BasicMetadataValueEnum> = Vec::new();
            if bound {
                args.push(adapter.get_nth_param(0).unwrap().into());
            }
            args.extend(adapter.get_param_iter().skip(1).map(BasicMetadataValueEnum::from));
            let call = this.b.build_call(target, &args, "").must();
            if func != this.program.entry {
                call.set_call_convention(crate::CALLCONV_FAST);
            }
            match call.try_as_basic_value().basic() {
                Some(value) => this.b.build_return(Some(&value)).must(),
                None => this.b.build_return(None).must(),
            };
        });
        adapter
    }

    // ---- control-flow values ------------------------------------------------------------

    /// `cond ? then() : otherwise()`, joined with a phi. Values of type `None` produce nothing.
    pub(crate) fn choose(
        &mut self,
        cond: IntValue<'ctx>,
        ty: Ty,
        then: impl FnOnce(&mut Self) -> Option<BasicValueEnum<'ctx>>,
        otherwise: impl FnOnce(&mut Self) -> Option<BasicValueEnum<'ctx>>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let then_block = self.block("then");
        let else_block = self.block("else");
        let merge = self.block("endchoose");
        self.b.build_conditional_branch(cond, then_block, else_block).must();
        self.b.position_at_end(then_block);
        let a = then(self);
        let a_end = self.b.get_insert_block().unwrap();
        self.b.build_unconditional_branch(merge).must();
        self.b.position_at_end(else_block);
        let b = otherwise(self);
        let b_end = self.b.get_insert_block().unwrap();
        self.b.build_unconditional_branch(merge).must();
        self.b.position_at_end(merge);
        let llty = self.llty(ty)?;
        let phi = self.b.build_phi(llty, "").must();
        phi.add_incoming(&[(&a.expect("branch value"), a_end), (&b.expect("branch value"), b_end)]);
        Some(phi.as_basic_value())
    }

    // ---- optionals ----------------------------------------------------------------------

    fn option_inner(&self, option: Ty) -> Ty {
        match self.kind(option) {
            TyKind::Option(inner) => *inner,
            other => unreachable!("not an optional type: {other:?}"),
        }
    }

    /// `None` of an optional type: all zeros, which is a null pointer for the nullable kinds and
    /// a false flag for the others.
    pub(crate) fn none_value(&self, option: Ty) -> BasicValueEnum<'ctx> {
        self.value_type(option).const_zero()
    }

    pub(crate) fn wrap_some(&mut self, option: Ty, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        let inner = self.option_inner(option);
        if self.option_is_nullable(inner) {
            return value;
        }
        let struct_type = self.value_type(option).into_struct_type();
        let with_flag = self.b.build_insert_value(struct_type.get_undef(), self.bool(true), 0, "").must();
        self.b.build_insert_value(with_flag, value, 1, "").must().into_struct_value().into()
    }

    pub(crate) fn opt_is_some(&mut self, option: Ty, value: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let inner = self.option_inner(option);
        if self.is_pointer_pair(inner) {
            let ptr = self.b.build_extract_value(value.into_struct_value(), 0, "").must().into_pointer_value();
            return self.b.build_is_not_null(ptr, "").must();
        }
        if self.option_is_nullable(inner) {
            return self.b.build_is_not_null(value.into_pointer_value(), "").must();
        }
        self.b.build_extract_value(value.into_struct_value(), 0, "").must().into_int_value()
    }

    pub(crate) fn opt_inner(&mut self, option: Ty, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        if self.option_is_nullable(self.option_inner(option)) {
            return value;
        }
        self.b.build_extract_value(value.into_struct_value(), 1, "").must()
    }

    // ---- tuples, lists, dicts -----------------------------------------------------------

    pub(crate) fn make_tuple(&mut self, values: &[BasicValueEnum<'ctx>]) -> BasicValueEnum<'ctx> {
        let types: Vec<_> = values.iter().map(|v| v.get_type()).collect();
        let tuple_type = self.cx.struct_type(&types, false);
        let mut tuple = tuple_type.get_undef();
        for (index, value) in values.iter().enumerate() {
            tuple = self.b.build_insert_value(tuple, *value, index as u32, "").must().into_struct_value();
        }
        tuple.into()
    }

    pub(crate) fn list_len(&mut self, list: PointerValue<'ctx>) -> IntValue<'ctx> {
        let ptr = self.b.build_struct_gep(self.list_type, list, 1, "").must();
        self.load_tagged(self.cx.i64_type(), ptr, "list.len", "len").into_int_value()
    }

    pub(crate) fn dict_len(&mut self, dict: PointerValue<'ctx>) -> IntValue<'ctx> {
        let ptr = self.b.build_struct_gep(self.dict_type, dict, 1, "").must();
        self.b.build_load(self.cx.i64_type(), ptr, "len").must().into_int_value()
    }

    fn list_data(&mut self, list: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let ptr = self.b.build_struct_gep(self.list_type, list, 3, "").must();
        self.load_tagged(self.ptr_type(), ptr, "list.data", "data").into_pointer_value()
    }

    pub(crate) fn list_elem_unchecked(
        &mut self,
        list: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        elem: Ty,
    ) -> BasicValueEnum<'ctx> {
        let data = self.list_data(list);
        let elem_type = self.value_type(elem);
        // SAFETY (LLVM): the caller has checked that `index` is within the list.
        let ptr = unsafe { self.b.build_in_bounds_gep(elem_type, data, &[index], "").must() };
        self.load_tagged(elem_type, ptr, "list.elem", "")
    }

    /// Address of `list[index]`, with Python's negative indexing and a bounds check.
    pub(crate) fn list_elem_ptr(
        &mut self,
        list: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        elem: Ty,
        span: Span,
    ) -> PointerValue<'ctx> {
        let len = self.list_len(list);
        let negative = self.b.build_int_compare(IntPredicate::SLT, index, self.i64(0), "").must();
        let wrapped = self.b.build_int_add(index, len, "").must();
        let adjusted = self.b.build_select(negative, wrapped, index, "").must().into_int_value();
        // Unsigned comparison also catches indexes still negative after adjustment.
        let out_of_range = self.b.build_int_compare(IntPredicate::UGE, adjusted, len, "").must();
        self.check(out_of_range, |this| {
            let (loc, loc_len) = this.location(span);
            this.call_rt("lrt_panic_index", &[index.into(), len.into(), loc.into(), loc_len.into()]);
        });
        let data = self.list_data(list);
        // SAFETY (LLVM): `adjusted` was checked against the length above.
        unsafe { self.b.build_in_bounds_gep(self.value_type(elem), data, &[adjusted], "").must() }
    }

    /// Append `value`. When the buffer has room, this is inline: a compare, a store and a
    /// length update; otherwise `lrt_list_push` grows the buffer and returns the new slot.
    pub(crate) fn list_push(&mut self, list: PointerValue<'ctx>, value: BasicValueEnum<'ctx>) {
        let i64_type = self.cx.i64_type();
        let len_ptr = self.b.build_struct_gep(self.list_type, list, 1, "").must();
        let cap_ptr = self.b.build_struct_gep(self.list_type, list, 2, "").must();
        let len = self.load_tagged(i64_type, len_ptr, "list.len", "len").into_int_value();
        let cap = self.load_tagged(i64_type, cap_ptr, "list.cap", "cap").into_int_value();
        let has_room = self.b.build_int_compare(IntPredicate::SLT, len, cap, "").must();
        let fast = self.block("push.fast");
        let slow = self.block("push.slow");
        let join = self.block("push.join");
        self.b.build_conditional_branch(has_room, fast, slow).must();

        self.b.position_at_end(fast);
        let data = self.list_data(list);
        // SAFETY (LLVM): `len < cap`, so the slot is inside the buffer.
        let fast_slot = unsafe { self.b.build_in_bounds_gep(value.get_type(), data, &[len], "").must() };
        let next = self.b.build_int_add(len, self.i64(1), "").must();
        self.store_tagged(len_ptr, next.into(), "list.len");
        self.b.build_unconditional_branch(join).must();

        self.b.position_at_end(slow);
        let slow_slot = self.call_rt("lrt_list_push", &[list.into()]).unwrap().into_pointer_value();
        self.b.build_unconditional_branch(join).must();

        self.b.position_at_end(join);
        let slot = self.b.build_phi(self.ptr_type(), "slot").must();
        slot.add_incoming(&[(&fast_slot as &dyn BasicValue, fast), (&slow_slot, slow)]);
        self.store_tagged(slot.as_basic_value().into_pointer_value(), value, "list.elem");
    }

    /// Address of the value for `key`; panics, showing the key, if it is absent.
    pub(crate) fn dict_lookup(
        &mut self,
        dict: PointerValue<'ctx>,
        key: BasicValueEnum<'ctx>,
        key_ty: Ty,
        span: Span,
    ) -> PointerValue<'ctx> {
        let key_ptr = self.spill(key);
        let found = self.call_rt("lrt_dict_find", &[dict.into(), key_ptr.into()]).unwrap().into_pointer_value();
        let missing = self.b.build_is_null(found, "").must();
        self.check(missing, |this| {
            let builder = this.builder_new();
            this.push_text(builder, "key ");
            this.push_value(builder, key_ty, Some(key), true);
            this.push_text(builder, " is not in the dict");
            let message = this.call_rt("lrt_builder_finish", &[builder.into()]).unwrap();
            let [ptr, len] = this.str_parts(message);
            let (loc, loc_len) = this.location(span);
            this.call_rt("lrt_panic", &[ptr, len, loc.into(), loc_len.into()]);
        });
        found
    }

    // ---- operators ----------------------------------------------------------------------

    fn binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> BasicValueEnum<'ctx> {
        let operand_ty = lhs.ty;
        if matches!(op, BinaryOp::Is | BinaryOp::IsNot) && matches!(rhs.kind, ExprKind::None) {
            let value = self.value(lhs);
            let some = self.opt_is_some(operand_ty, value);
            return if op == BinaryOp::IsNot { some.into() } else { self.b.build_not(some, "").must().into() };
        }
        if op == BinaryOp::StrConcat
            && let ExprKind::Binary { op: BinaryOp::StrConcat, .. } = lhs.kind
        {
            return self.concat_chain(lhs, rhs);
        }
        let l = self.value(lhs);
        let r = self.value(rhs);
        self.binary_values(op, l, r, operand_ty, rhs.ty, span)
    }

    /// `a + b + c + ...` on strings, three or more parts: evaluated left to right as written,
    /// then joined by one runtime call that allocates the result once at its full length,
    /// instead of one new string per `+` (docs/09, phase 4).
    fn concat_chain(&mut self, lhs: &Expr, rhs: &Expr) -> BasicValueEnum<'ctx> {
        let mut parts = vec![rhs];
        let mut left = lhs;
        while let ExprKind::Binary { op: BinaryOp::StrConcat, lhs, rhs } = &left.kind {
            parts.push(rhs);
            left = lhs;
        }
        parts.push(left);
        parts.reverse();
        let values: Vec<BasicValueEnum> = parts.iter().map(|part| self.value(part)).collect();
        let array_type = self.str_type.array_type(values.len() as u32);
        let array = self.alloca(array_type.into(), "parts");
        for (index, value) in values.iter().enumerate() {
            let zero = self.i64(0);
            let at = self.i64(index as i64);
            // SAFETY (LLVM): within the array of `values.len()` strings.
            let slot = unsafe { self.b.build_in_bounds_gep(array_type, array, &[zero, at], "").must() };
            self.b.build_store(slot, *value).must();
        }
        let count = self.i64(values.len() as i64);
        self.call_rt("lrt_str_concat_n", &[array.into(), count.into()]).unwrap()
    }

    /// A binary operation on values already computed.
    pub(crate) fn binary_values(
        &mut self,
        op: BinaryOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
        operand_ty: Ty,
        rhs_ty: Ty,
        span: Span,
    ) -> BasicValueEnum<'ctx> {
        let b = &self.b;
        let int = |v: BasicValueEnum<'ctx>| v.into_int_value();
        let float = |v: BasicValueEnum<'ctx>| v.into_float_value();
        match op {
            BinaryOp::IntAdd => b.build_int_add(int(l), int(r), "").must().into(),
            BinaryOp::IntSub => b.build_int_sub(int(l), int(r), "").must().into(),
            BinaryOp::IntMul => b.build_int_mul(int(l), int(r), "").must().into(),
            BinaryOp::IntAnd => b.build_and(int(l), int(r), "").must().into(),
            BinaryOp::IntOr => b.build_or(int(l), int(r), "").must().into(),
            BinaryOp::IntXor => b.build_xor(int(l), int(r), "").must().into(),
            BinaryOp::IntFloorDiv | BinaryOp::IntMod => self.int_div(op, int(l), int(r), span).into(),
            BinaryOp::IntShl | BinaryOp::IntShr => self.shift(op, int(l), int(r), span).into(),
            BinaryOp::IntPow => {
                let (loc, loc_len) = self.location(span);
                self.call_rt("lrt_int_pow", &[l, r, loc.into(), loc_len.into()]).unwrap()
            }
            BinaryOp::IntTrueDiv => {
                self.check_nonzero(int(r), span);
                let f64_type = self.cx.f64_type();
                let lf = self.b.build_signed_int_to_float(int(l), f64_type, "").must();
                let rf = self.b.build_signed_int_to_float(int(r), f64_type, "").must();
                self.b.build_float_div(lf, rf, "").must().into()
            }
            BinaryOp::FloatAdd => b.build_float_add(float(l), float(r), "").must().into(),
            BinaryOp::FloatSub => b.build_float_sub(float(l), float(r), "").must().into(),
            BinaryOp::FloatMul => b.build_float_mul(float(l), float(r), "").must().into(),
            BinaryOp::FloatDiv => b.build_float_div(float(l), float(r), "").must().into(),
            BinaryOp::FloatFloorDiv => {
                let quotient = self.b.build_float_div(float(l), float(r), "").must();
                self.call_rt("llvm.floor.f64", &[quotient.into()]).unwrap()
            }
            BinaryOp::FloatMod => {
                // Python's `%` takes the sign of the divisor.
                let rem = self.b.build_float_rem(float(l), float(r), "").must();
                let zero = self.cx.f64_type().const_zero();
                let nonzero = self.b.build_float_compare(FloatPredicate::ONE, rem, zero, "").must();
                let rem_negative = self.b.build_float_compare(FloatPredicate::OLT, rem, zero, "").must();
                let divisor_negative = self.b.build_float_compare(FloatPredicate::OLT, float(r), zero, "").must();
                let signs_differ = self.b.build_xor(rem_negative, divisor_negative, "").must();
                let adjust = self.b.build_and(nonzero, signs_differ, "").must();
                let adjusted = self.b.build_float_add(rem, float(r), "").must();
                self.b.build_select(adjust, adjusted, rem, "").must()
            }
            BinaryOp::FloatPow => self.call_rt("llvm.pow.f64", &[l, r]).unwrap(),
            BinaryOp::StrConcat => {
                let [lp, ll] = self.str_parts(l);
                let [rp, rl] = self.str_parts(r);
                self.call_rt("lrt_str_concat", &[lp, ll, rp, rl]).unwrap()
            }
            BinaryOp::ListConcat => self.call_rt("lrt_list_concat", &[l, r]).unwrap(),
            BinaryOp::StrRepeat => {
                let [ptr, len] = self.str_parts(l);
                self.call_rt("lrt_str_repeat", &[ptr, len, r]).unwrap()
            }
            BinaryOp::Eq => self.emit_eq(operand_ty, l, r).into(),
            BinaryOp::NotEq => {
                let eq = self.emit_eq(operand_ty, l, r);
                self.b.build_not(eq, "").must().into()
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => self.compare(op, operand_ty, l, r).into(),
            BinaryOp::Is | BinaryOp::IsNot => {
                let i64_type = self.cx.i64_type();
                let a = self.b.build_ptr_to_int(l.into_pointer_value(), i64_type, "").must();
                let c = self.b.build_ptr_to_int(r.into_pointer_value(), i64_type, "").must();
                let predicate = if op == BinaryOp::Is { IntPredicate::EQ } else { IntPredicate::NE };
                self.b.build_int_compare(predicate, a, c, "").must().into()
            }
            BinaryOp::In | BinaryOp::NotIn => {
                let found = self.contains(l, operand_ty, r, rhs_ty);
                if op == BinaryOp::NotIn { self.b.build_not(found, "").must().into() } else { found.into() }
            }
        }
    }

    fn check_nonzero(&mut self, divisor: IntValue<'ctx>, span: Span) {
        if divisor.get_sign_extended_constant().is_some_and(|c| c != 0) {
            return;
        }
        let zero = self.b.build_int_compare(IntPredicate::EQ, divisor, self.i64(0), "").must();
        self.check(zero, |this| this.panic_message("division by zero", span));
    }

    /// Python's floor division and modulo: the quotient rounds down and the remainder takes the
    /// divisor's sign. `MIN // -1` wraps, like other overflow.
    fn int_div(&mut self, op: BinaryOp, l: IntValue<'ctx>, r: IntValue<'ctx>, span: Span) -> IntValue<'ctx> {
        self.check_nonzero(r, span);
        let b = &self.b;
        let minus_one = self.i64(-1);
        let zero = self.i64(0);
        // `sdiv` of MIN by -1 is undefined in LLVM, so divide by 1 instead and fix up after.
        let is_minus_one = b.build_int_compare(IntPredicate::EQ, r, minus_one, "").must();
        let safe = b.build_select(is_minus_one, self.i64(1), r, "").must().into_int_value();
        let quotient = b.build_int_signed_div(l, safe, "").must();
        let remainder = b.build_int_signed_rem(l, safe, "").must();
        let negated = b.build_int_neg(l, "").must();
        let quotient = b.build_select(is_minus_one, negated, quotient, "").must().into_int_value();
        let has_remainder = b.build_int_compare(IntPredicate::NE, remainder, zero, "").must();
        let signs = b.build_xor(remainder, r, "").must();
        let signs_differ = b.build_int_compare(IntPredicate::SLT, signs, zero, "").must();
        let adjust = b.build_and(has_remainder, signs_differ, "").must();
        if op == BinaryOp::IntFloorDiv {
            let step = b.build_int_z_extend(adjust, self.cx.i64_type(), "").must();
            b.build_int_sub(quotient, step, "").must()
        } else {
            let adjusted = b.build_int_add(remainder, r, "").must();
            b.build_select(adjust, adjusted, remainder, "").must().into_int_value()
        }
    }

    /// Shifts by 64 or more give 0 (or -1 for negative numbers shifted right), as Python's
    /// unbounded integers would before wrapping; negative counts panic.
    fn shift(&mut self, op: BinaryOp, l: IntValue<'ctx>, count: IntValue<'ctx>, span: Span) -> IntValue<'ctx> {
        let negative = self.b.build_int_compare(IntPredicate::SLT, count, self.i64(0), "").must();
        self.check(negative, |this| this.panic_message("negative shift count", span));
        let b = &self.b;
        let too_big = b.build_int_compare(IntPredicate::SGT, count, self.i64(63), "").must();
        let clamped = b.build_select(too_big, self.i64(63), count, "").must().into_int_value();
        if op == BinaryOp::IntShl {
            let shifted = b.build_left_shift(l, clamped, "").must();
            b.build_select(too_big, self.i64(0), shifted, "").must().into_int_value()
        } else {
            b.build_right_shift(l, clamped, true, "").must()
        }
    }

    /// `a == b` for a value type; composite types use a generated helper.
    pub(crate) fn emit_eq(&mut self, ty: Ty, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        match self.kind(ty) {
            TyKind::Int | TyKind::Bool => {
                self.b.build_int_compare(IntPredicate::EQ, a.into_int_value(), b.into_int_value(), "").must()
            }
            TyKind::Float => {
                self.b.build_float_compare(FloatPredicate::OEQ, a.into_float_value(), b.into_float_value(), "").must()
            }
            TyKind::Str => {
                let [ap, al] = self.str_parts(a);
                let [bp, bl] = self.str_parts(b);
                self.str_eq(ap, al, bp, bl)
            }
            TyKind::None => self.bool(true),
            TyKind::Sized(kind) if kind.is_float() => {
                self.b.build_float_compare(FloatPredicate::OEQ, a.into_float_value(), b.into_float_value(), "").must()
            }
            TyKind::Sized(_) => {
                self.b.build_int_compare(IntPredicate::EQ, a.into_int_value(), b.into_int_value(), "").must()
            }
            TyKind::Struct(_) | TyKind::Enum(_) => {
                let eq = self.program.builtins.eq;
                if let Some(function) = self.explicit_method(eq, ty, 0) {
                    return self.call_method(function, &[a, b]).unwrap().into_int_value();
                }
                let helper = self.helper(Helper::Eq, ty);
                let call = self.b.build_call(helper, &[a.into(), b.into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
                call.try_as_basic_value().basic().unwrap().into_int_value()
            }
            TyKind::Dict(_, value_ty) => {
                let value_ty = *value_ty;
                let eq = self.helper(Helper::EqPtr, value_ty).as_global_value().as_pointer_value();
                let result = self.call_rt("lrt_dict_eq", &[a, b, eq.into()]).unwrap().into_int_value();
                self.b.build_int_compare(IntPredicate::NE, result, self.i64(0), "").must()
            }
            TyKind::Dyn(_)
            | TyKind::SelfType(_)
            | TyKind::Func(..)
            | TyKind::Task(_)
            | TyKind::Channel(_)
            | TyKind::Mutex(_)
            | TyKind::Guard(_)
            | TyKind::Atomic => {
                let i64_type = self.cx.i64_type();
                let x = self.b.build_ptr_to_int(a.into_pointer_value(), i64_type, "").must();
                let y = self.b.build_ptr_to_int(b.into_pointer_value(), i64_type, "").must();
                self.b.build_int_compare(IntPredicate::EQ, x, y, "").must()
            }
            TyKind::List(_) | TyKind::Tuple(_) | TyKind::Option(_) | TyKind::Result(..) => {
                let helper = self.helper(Helper::Eq, ty);
                let call = self.b.build_call(helper, &[a.into(), b.into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
                call.try_as_basic_value().basic().unwrap().into_int_value()
            }
            TyKind::Error | TyKind::Param(_) | TyKind::FrozenList(_) | TyKind::FrozenDict(..) => unreachable!(),
        }
    }

    /// `<`, `<=`, `>`, `>=`: directly on numbers, booleans and strings; through a three-way
    /// comparison on tuples, lists and types with `Ord`.
    fn compare(&mut self, op: BinaryOp, ty: Ty, a: BasicValueEnum<'ctx>, b: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let (signed, unsigned, float) = match op {
            BinaryOp::Lt => (IntPredicate::SLT, IntPredicate::ULT, FloatPredicate::OLT),
            BinaryOp::Le => (IntPredicate::SLE, IntPredicate::ULE, FloatPredicate::OLE),
            BinaryOp::Gt => (IntPredicate::SGT, IntPredicate::UGT, FloatPredicate::OGT),
            _ => (IntPredicate::SGE, IntPredicate::UGE, FloatPredicate::OGE),
        };
        match self.kind(ty) {
            TyKind::Int => self.b.build_int_compare(signed, a.into_int_value(), b.into_int_value(), "").must(),
            TyKind::Bool => self.b.build_int_compare(unsigned, a.into_int_value(), b.into_int_value(), "").must(),
            TyKind::Float => self.b.build_float_compare(float, a.into_float_value(), b.into_float_value(), "").must(),
            TyKind::Sized(kind) if kind.is_float() => {
                self.b.build_float_compare(float, a.into_float_value(), b.into_float_value(), "").must()
            }
            TyKind::Sized(kind) => {
                let predicate = if kind.is_signed() { signed } else { unsigned };
                self.b.build_int_compare(predicate, a.into_int_value(), b.into_int_value(), "").must()
            }
            TyKind::Str => {
                let [ap, al] = self.str_parts(a);
                let [bp, bl] = self.str_parts(b);
                let cmp = self.call_rt("lrt_str_cmp", &[ap, al, bp, bl]).unwrap().into_int_value();
                self.b.build_int_compare(signed, cmp, self.i64(0), "").must()
            }
            _ => {
                let cmp = self.emit_cmp(ty, a, b);
                self.b.build_int_compare(signed, cmp, self.i64(0), "").must()
            }
        }
    }

    /// `needle in container` for strings, lists and dicts.
    fn contains(
        &mut self,
        needle: BasicValueEnum<'ctx>,
        needle_ty: Ty,
        container: BasicValueEnum<'ctx>,
        container_ty: Ty,
    ) -> IntValue<'ctx> {
        match self.kind(container_ty) {
            TyKind::Str => {
                let [hp, hl] = self.str_parts(container);
                let [np, nl] = self.str_parts(needle);
                let at = self.call_rt("lrt_str_find", &[hp, hl, np, nl]).unwrap().into_int_value();
                self.b.build_int_compare(IntPredicate::SGE, at, self.i64(0), "").must()
            }
            TyKind::List(_) => {
                let eq = self.helper(Helper::EqPtr, needle_ty).as_global_value().as_pointer_value();
                let ptr = self.spill(needle);
                let at =
                    self.call_rt("lrt_list_index_of", &[container, ptr.into(), eq.into()]).unwrap().into_int_value();
                self.b.build_int_compare(IntPredicate::SGE, at, self.i64(0), "").must()
            }
            TyKind::Dict(..) => {
                let ptr = self.spill(needle);
                let found = self.call_rt("lrt_dict_find", &[container, ptr.into()]).unwrap().into_pointer_value();
                self.b.build_is_not_null(found, "").must()
            }
            other => unreachable!("`in` on {other:?}"),
        }
    }

    // ---- text ---------------------------------------------------------------------------

    /// String equality, inline: equal lengths, then `memcmp` of the bytes, which LLVM expands
    /// into a few compares when one side is a constant.
    pub(crate) fn str_eq(
        &mut self,
        ap: BasicValueEnum<'ctx>,
        al: BasicValueEnum<'ctx>,
        bp: BasicValueEnum<'ctx>,
        bl: BasicValueEnum<'ctx>,
    ) -> IntValue<'ctx> {
        let memcmp = self.module.get_function("memcmp").unwrap_or_else(|| {
            let ptr = self.ptr_type();
            let fn_type = self.cx.i32_type().fn_type(&[ptr.into(), ptr.into(), self.cx.i64_type().into()], false);
            self.module.add_function("memcmp", fn_type, Some(inkwell::module::Linkage::External))
        });
        let same_len = self.b.build_int_compare(IntPredicate::EQ, al.into_int_value(), bl.into_int_value(), "").must();
        let start = self.b.get_insert_block().expect("inside a block");
        let compare = self.block("streq.bytes");
        let done = self.block("streq.done");
        self.b.build_conditional_branch(same_len, compare, done).must();
        self.b.position_at_end(compare);
        let order = self.b.build_call(memcmp, &[ap.into(), bp.into(), al.into()], "").must();
        let order = order.try_as_basic_value().basic().expect("memcmp returns a value").into_int_value();
        let bytes_equal = self.b.build_int_compare(IntPredicate::EQ, order, self.cx.i32_type().const_zero(), "").must();
        let compare_end = self.b.get_insert_block().expect("inside a block");
        self.b.build_unconditional_branch(done).must();
        self.b.position_at_end(done);
        let phi = self.b.build_phi(self.cx.bool_type(), "streq").must();
        phi.add_incoming(&[(&self.bool(false) as &dyn BasicValue, start), (&bytes_equal, compare_end)]);
        phi.as_basic_value().into_int_value()
    }

    /// A new, empty string builder (`lang_rt::builder::Builder`: data, length, capacity). It lives
    /// in this function's frame, not on the heap: each use site gets its own slot, cleared here,
    /// and a builder never outlives the expression that creates it.
    pub(crate) fn builder_new(&mut self) -> PointerValue<'ctx> {
        let i64_type = self.cx.i64_type();
        let builder_type = self.cx.struct_type(&[self.ptr_type().into(), i64_type.into(), i64_type.into()], false);
        let slot = self.alloca(builder_type.into(), "builder");
        self.b.build_store(slot, builder_type.const_zero()).must();
        slot
    }

    /// A builder with room for `capacity` bytes, when that is a useful guess.
    pub(crate) fn builder_with_capacity(&mut self, capacity: u64) -> PointerValue<'ctx> {
        let builder = self.builder_new();
        if capacity > 0 {
            self.call_rt("lrt_builder_reserve", &[builder.into(), self.i64(capacity as i64).into()]);
        }
        builder
    }

    /// A guess at an f-string's length: its literal text, plus a typical width per value. A low
    /// guess costs a reallocation; a high one, unused bytes in the string.
    fn fstring_estimate(&self, parts: &[FPart]) -> u64 {
        parts
            .iter()
            .map(|part| match part {
                FPart::Literal(text) => text.len() as u64,
                FPart::Value { expr, .. } => match self.kind(expr.ty) {
                    TyKind::Int | TyKind::Sized(_) => 10,
                    TyKind::Float => 12,
                    TyKind::Bool => 5,
                    _ => 16,
                },
            })
            .sum()
    }

    pub(crate) fn push_text(&mut self, builder: PointerValue<'ctx>, text: &str) {
        let value = self.str_const(text);
        let [ptr, len] = self.str_parts(value.into());
        self.call_rt("lrt_builder_push", &[builder.into(), ptr, len]);
    }

    /// Append how a value prints. With `repr`, strings are quoted, as inside containers.
    pub(crate) fn push_value(
        &mut self,
        builder: PointerValue<'ctx>,
        ty: Ty,
        value: Option<BasicValueEnum<'ctx>>,
        repr: bool,
    ) {
        match self.kind(ty) {
            TyKind::None => self.push_text(builder, "None"),
            TyKind::Int => {
                self.call_rt("lrt_builder_push_int", &[builder.into(), value.unwrap()]);
            }
            TyKind::Float => {
                self.call_rt("lrt_builder_push_float", &[builder.into(), value.unwrap()]);
            }
            TyKind::Sized(kind) => {
                let wide = self.widen(*kind, value.unwrap());
                let function = match (kind.is_float(), kind.is_signed()) {
                    (true, _) => "lrt_builder_push_float",
                    (false, true) => "lrt_builder_push_int",
                    (false, false) => "lrt_builder_push_uint",
                };
                self.call_rt(function, &[builder.into(), wide]);
            }
            TyKind::Bool => {
                let truth = self.bytes_const("True");
                let falsehood = self.bytes_const("False");
                let cond = value.unwrap().into_int_value();
                let ptr = self.b.build_select(cond, truth, falsehood, "").must();
                let len = self.b.build_select(cond, self.i64(4), self.i64(5), "").must();
                self.call_rt("lrt_builder_push", &[builder.into(), ptr, len]);
            }
            TyKind::Str => {
                let [ptr, len] = self.str_parts(value.unwrap());
                let function = if repr { "lrt_builder_push_str_repr" } else { "lrt_builder_push" };
                self.call_rt(function, &[builder.into(), ptr, len]);
            }
            TyKind::Struct(_) | TyKind::Enum(_) | TyKind::Dyn(_) | TyKind::SelfType(_) => {
                self.push_user_value(builder, ty, value.unwrap(), repr);
            }
            _ => {
                let helper = self.helper(Helper::Push { repr }, ty);
                let call = self.b.build_call(helper, &[builder.into(), value.unwrap().into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
            }
        }
    }

    /// A sized number as an `int` (sign- or zero-extended) or, for `f32`, a `float`.
    pub(crate) fn widen(
        &mut self,
        kind: lang_sema::types::SizedKind,
        value: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if kind.is_float() {
            return self.b.build_float_ext(value.into_float_value(), self.cx.f64_type(), "").must().into();
        }
        let i64_type = self.cx.i64_type();
        if kind.bits() == 64 {
            return value;
        }
        if kind.is_signed() {
            self.b.build_int_s_extend(value.into_int_value(), i64_type, "").must().into()
        } else {
            self.b.build_int_z_extend(value.into_int_value(), i64_type, "").must().into()
        }
    }

    /// Convert a number between `int`, `float` and the sized types. Integers wrap; floats
    /// truncate toward zero, saturating at the limits of `int` (NaN becomes 0).
    pub(crate) fn num_convert(&mut self, from: Ty, to: Ty, value: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        use lang_sema::types::SizedKind;
        // (is a float, bits, signed)
        let shape = |this: &Self, ty: Ty| -> (bool, u32, bool) {
            match this.kind(ty) {
                TyKind::Int => (false, 64, true),
                TyKind::Float => (true, 64, true),
                TyKind::Sized(k) => (k.is_float(), k.bits(), k.is_signed()),
                other => unreachable!("not a number: {other:?}"),
            }
        };
        let (from_float, from_bits, from_signed) = shape(self, from);
        let (to_float, to_bits, _) = shape(self, to);
        let target = self.value_type(to);
        match (from_float, to_float) {
            (false, false) => {
                let v = value.into_int_value();
                let t = target.into_int_type();
                if to_bits < from_bits {
                    self.b.build_int_truncate(v, t, "").must().into()
                } else if to_bits > from_bits && from_signed {
                    self.b.build_int_s_extend(v, t, "").must().into()
                } else if to_bits > from_bits {
                    self.b.build_int_z_extend(v, t, "").must().into()
                } else {
                    v.into()
                }
            }
            (false, true) => {
                let v = value.into_int_value();
                let t = target.into_float_type();
                if from_signed {
                    self.b.build_signed_int_to_float(v, t, "").must().into()
                } else {
                    self.b.build_unsigned_int_to_float(v, t, "").must().into()
                }
            }
            (true, false) => {
                let wide = if from_bits == 32 {
                    self.b.build_float_ext(value.into_float_value(), self.cx.f64_type(), "").must().into()
                } else {
                    value
                };
                let as_int = self.call_rt("llvm.fptosi.sat.i64.f64", &[wide]).unwrap();
                let _ = SizedKind::ALL;
                self.num_convert(Ty::INT, to, as_int)
            }
            (true, true) => {
                let v = value.into_float_value();
                let t = target.into_float_type();
                if to_bits < from_bits {
                    self.b.build_float_trunc(v, t, "").must().into()
                } else if to_bits > from_bits {
                    self.b.build_float_ext(v, t, "").must().into()
                } else {
                    v.into()
                }
            }
        }
    }

    /// The `str` form of any printable value.
    pub(crate) fn display_string(&mut self, ty: Ty, value: Option<BasicValueEnum<'ctx>>) -> BasicValueEnum<'ctx> {
        let builder = self.builder_new();
        self.push_value(builder, ty, value, false);
        self.call_rt("lrt_builder_finish", &[builder.into()]).unwrap()
    }
}
