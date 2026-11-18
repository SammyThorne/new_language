//! Statements and control flow.
//!
//! After a terminator (`return`, `break`, `continue`, a panic), emission continues in a fresh
//! "dead" block that nothing jumps to, so later code always has somewhere to go and LLVM removes
//! it.

use crate::{Codegen, Must};
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use lang_sema::hir::{Expr, LocalId, LoopSource, LoopTarget, Place, Stmt};
use lang_sema::types::{Ty, TyKind};

impl<'ctx> Codegen<'ctx, '_> {
    pub(crate) fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn slot(&self, local: LocalId) -> Option<PointerValue<'ctx>> {
        self.state.as_ref().expect("inside a function").slots[local.0]
    }

    /// Where a local's value is: its stack slot, or the heap cell the slot points to.
    fn local_ptr(&mut self, local: LocalId) -> Option<PointerValue<'ctx>> {
        let slot = self.slot(local)?;
        if self.state.as_ref().expect("inside a function").boxed[local.0] {
            return Some(self.b.build_load(self.ptr_type(), slot, "cell").must().into_pointer_value());
        }
        Some(slot)
    }

    /// The address of a boxed local's cell, for a closure's environment.
    pub(crate) fn local_cell(&mut self, local: LocalId) -> PointerValue<'ctx> {
        self.local_ptr(local).expect("captured variables have a value")
    }

    pub(crate) fn load_local(&mut self, local: LocalId, ty: Ty) -> Option<BasicValueEnum<'ctx>> {
        let ptr = self.local_ptr(local)?;
        Some(self.b.build_load(self.value_type(ty), ptr, "").must())
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Assign { place, value } => {
                let value = self.expr(value);
                self.store(place, value);
            }
            Stmt::Expr(expr) => {
                self.expr(expr);
            }
            Stmt::Return(value) => {
                let value = value.as_ref().and_then(|e| self.expr(e));
                self.run_cleanups(0);
                let returns_value = self.current_function().get_type().get_return_type().is_some();
                match value {
                    Some(value) if returns_value => self.b.build_return(Some(&value)).must(),
                    _ => self.b.build_return(None).must(),
                };
                self.start_dead_block();
            }
            Stmt::If { cond, then, otherwise } => {
                let cond = self.value(cond).into_int_value();
                let then_block = self.block("then");
                let else_block = self.block("else");
                let merge = self.block("endif");
                self.b.build_conditional_branch(cond, then_block, else_block).must();
                self.b.position_at_end(then_block);
                self.stmts(then);
                self.b.build_unconditional_branch(merge).must();
                self.b.position_at_end(else_block);
                self.stmts(otherwise);
                self.b.build_unconditional_branch(merge).must();
                self.b.position_at_end(merge);
            }
            Stmt::While { cond, body } => {
                let header = self.block("while");
                let body_block = self.block("body");
                let exit = self.block("endwhile");
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let cond = self.value(cond).into_int_value();
                self.b.build_conditional_branch(cond, body_block, exit).must();
                self.b.position_at_end(body_block);
                self.loop_body(body, header, exit);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
            Stmt::For { target, source, body } => self.for_loop(target, source, body),
            Stmt::Match { subject, cases } => self.match_stmt(subject, cases),
            Stmt::With { object, exit, body } => {
                let ty = self.program.functions[exit.0].locals[0].ty;
                self.state.as_mut().unwrap().cleanups.push((*object, *exit, ty));
                self.stmts(body);
                let depth = self.state.as_ref().unwrap().cleanups.len() - 1;
                self.run_cleanups(depth);
                self.state.as_mut().unwrap().cleanups.pop();
            }
            Stmt::Break | Stmt::Continue => {
                let &(continue_to, break_to, depth) = self.state.as_ref().unwrap().loops.last().expect("inside a loop");
                self.run_cleanups(depth);
                let to = if matches!(stmt, Stmt::Break) { break_to } else { continue_to };
                self.b.build_unconditional_branch(to).must();
                self.start_dead_block();
            }
        }
    }

    fn loop_body(
        &mut self,
        body: &[Stmt],
        continue_to: inkwell::basic_block::BasicBlock<'ctx>,
        break_to: inkwell::basic_block::BasicBlock<'ctx>,
    ) {
        let depth = self.state.as_ref().unwrap().cleanups.len();
        self.state.as_mut().unwrap().loops.push((continue_to, break_to, depth));
        self.stmts(body);
        self.state.as_mut().unwrap().loops.pop();
    }

    /// Call the `exit` method of every open `with` block above `depth`, innermost first. The
    /// blocks stay open for the code generator: this emits the calls for one way out.
    pub(crate) fn run_cleanups(&mut self, depth: usize) {
        let cleanups: Vec<_> = self.state.as_ref().unwrap().cleanups[depth..].iter().rev().copied().collect();
        for (object, exit, ty) in cleanups {
            let value = self.load_local(object, ty).expect("`with` values are objects");
            let call = self.b.build_call(self.function_value(exit), &[value.into()], "").must();
            call.set_call_convention(crate::CALLCONV_FAST);
        }
    }

    pub(crate) fn store(&mut self, place: &Place, value: Option<BasicValueEnum<'ctx>>) {
        let Some(value) = value else { return };
        match place {
            Place::Local(local) => {
                if let Some(ptr) = self.local_ptr(*local) {
                    self.b.build_store(ptr, value).must();
                }
            }
            Place::Field { object, index } => {
                let ptr = self.field_ptr(object, *index);
                self.b.build_store(ptr, value).must();
            }
            Place::Index { container, index, span } => {
                let container_value = self.value(container).into_pointer_value();
                let index_value = self.value(index);
                let ptr = match self.kind(container.ty) {
                    TyKind::List(elem) => {
                        self.list_elem_ptr(container_value, index_value.into_int_value(), *elem, *span)
                    }
                    TyKind::Dict(..) => {
                        let key = self.spill(index_value);
                        self.call_rt("lrt_dict_slot", &[container_value.into(), key.into()])
                            .unwrap()
                            .into_pointer_value()
                    }
                    other => unreachable!("cannot assign into {other:?}"),
                };
                self.b.build_store(ptr, value).must();
            }
        }
    }

    /// Address of a struct field.
    pub(crate) fn field_ptr(&mut self, object: &Expr, index: usize) -> PointerValue<'ctx> {
        let TyKind::Struct(id) = self.kind(object.ty) else { unreachable!("field of a non-struct") };
        let struct_type = self.struct_types[id.0];
        let object = self.value(object).into_pointer_value();
        self.b.build_struct_gep(struct_type, object, index as u32 + 1, "").must()
    }

    fn assign_target(&mut self, target: &LoopTarget, value: BasicValueEnum<'ctx>) {
        match target {
            LoopTarget::Local(local) => self.store(&Place::Local(*local), Some(value)),
            LoopTarget::Tuple(locals) => {
                let tuple = value.into_struct_value();
                for (index, local) in locals.iter().enumerate() {
                    let element = self.b.build_extract_value(tuple, index as u32, "").must();
                    self.store(&Place::Local(*local), Some(element));
                }
            }
        }
    }

    fn counter(&mut self, start: IntValue<'ctx>) -> PointerValue<'ctx> {
        let counter = self.alloca(self.cx.i64_type().into(), "counter");
        self.b.build_store(counter, start).must();
        counter
    }

    fn for_loop(&mut self, target: &LoopTarget, source: &LoopSource, body: &[Stmt]) {
        let i64_type = self.cx.i64_type();
        match source {
            LoopSource::Range { start, stop, step } => {
                let start = self.value(start).into_int_value();
                let stop = self.value(stop).into_int_value();
                let step_value = self.value(step).into_int_value();
                if step_value.get_sign_extended_constant().is_none_or(|c| c == 0) {
                    let zero = self.b.build_int_compare(IntPredicate::EQ, step_value, self.i64(0), "").must();
                    let span = step.span;
                    self.check(zero, |this| this.panic_message("range() step cannot be zero", span));
                }
                let counter = self.counter(start);
                let (header, body_block, latch, exit) =
                    (self.block("for"), self.block("body"), self.block("next"), self.block("endfor"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let i = self.b.build_load(i64_type, counter, "i").must().into_int_value();
                let upward = self.b.build_int_compare(IntPredicate::SGT, step_value, self.i64(0), "").must();
                let below = self.b.build_int_compare(IntPredicate::SLT, i, stop, "").must();
                let above = self.b.build_int_compare(IntPredicate::SGT, i, stop, "").must();
                let more = self.b.build_select(upward, below, above, "").must().into_int_value();
                self.b.build_conditional_branch(more, body_block, exit).must();
                self.b.position_at_end(body_block);
                self.assign_target(target, i.into());
                self.loop_body(body, latch, exit);
                self.b.build_unconditional_branch(latch).must();
                self.b.position_at_end(latch);
                let i = self.b.build_load(i64_type, counter, "").must().into_int_value();
                let next = self.b.build_int_add(i, step_value, "").must();
                self.b.build_store(counter, next).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
            LoopSource::List(list) => {
                let TyKind::List(elem) = *self.kind(list.ty) else { unreachable!() };
                let list = self.value(list).into_pointer_value();
                let counter = self.counter(self.i64(0));
                let (header, body_block, latch, exit) =
                    (self.block("for"), self.block("body"), self.block("next"), self.block("endfor"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let i = self.b.build_load(i64_type, counter, "i").must().into_int_value();
                let len = self.list_len(list);
                let more = self.b.build_int_compare(IntPredicate::SLT, i, len, "").must();
                self.b.build_conditional_branch(more, body_block, exit).must();
                self.b.position_at_end(body_block);
                let element = self.list_elem_unchecked(list, i, elem);
                self.assign_target(target, element);
                self.loop_body(body, latch, exit);
                self.b.build_unconditional_branch(latch).must();
                self.b.position_at_end(latch);
                let i = self.b.build_load(i64_type, counter, "").must().into_int_value();
                let next = self.b.build_int_add(i, self.i64(1), "").must();
                self.b.build_store(counter, next).must();
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
            LoopSource::Str(text) => {
                let text = self.value(text);
                let [ptr, len] = self.str_parts(text);
                let offset = self.counter(self.i64(0));
                let (header, body_block, exit) = (self.block("for"), self.block("body"), self.block("endfor"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let ch = self.call_rt("lrt_str_next_char", &[ptr, len, offset.into()]).unwrap();
                let ch_ptr = self.b.build_extract_value(ch.into_struct_value(), 0, "").must().into_pointer_value();
                let done = self.b.build_is_null(ch_ptr, "").must();
                self.b.build_conditional_branch(done, exit, body_block).must();
                self.b.position_at_end(body_block);
                self.assign_target(target, ch);
                self.loop_body(body, header, exit);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
            LoopSource::Iterator { iterator, next } => {
                // Call `next` until it returns `None`.
                let iterator_value = self.value(iterator);
                let next_function = self.function_value(*next);
                let option_ty = self.program.functions[next.0].ret;
                let (header, body_block, exit) = (self.block("for"), self.block("body"), self.block("endfor"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let call = self.b.build_call(next_function, &[iterator_value.into()], "").must();
                call.set_call_convention(crate::CALLCONV_FAST);
                let item = call.try_as_basic_value().basic().expect("`next` returns an optional");
                let present = self.opt_is_some(option_ty, item);
                self.b.build_conditional_branch(present, body_block, exit).must();
                self.b.position_at_end(body_block);
                let element = self.opt_inner(option_ty, item);
                self.assign_target(target, element);
                self.loop_body(body, header, exit);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
            LoopSource::DictKeys(dict) | LoopSource::DictValues(dict) | LoopSource::DictItems(dict) => {
                let TyKind::Dict(key_ty, value_ty) = *self.kind(dict.ty) else { unreachable!() };
                let dict_value = self.value(dict).into_pointer_value();
                let cursor = self.counter(self.i64(0));
                let (header, body_block, exit) = (self.block("for"), self.block("body"), self.block("endfor"));
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(header);
                let entry =
                    self.call_rt("lrt_dict_next", &[dict_value.into(), cursor.into()]).unwrap().into_pointer_value();
                let done = self.b.build_is_null(entry, "").must();
                self.b.build_conditional_branch(done, exit, body_block).must();
                self.b.position_at_end(body_block);
                let element = match source {
                    LoopSource::DictKeys(_) => self.entry_key(entry, key_ty),
                    LoopSource::DictValues(_) => self.entry_value(entry, key_ty, value_ty),
                    _ => {
                        let key = self.entry_key(entry, key_ty);
                        let value = self.entry_value(entry, key_ty, value_ty);
                        self.make_tuple(&[key, value])
                    }
                };
                self.assign_target(target, element);
                self.loop_body(body, header, exit);
                self.b.build_unconditional_branch(header).must();
                self.b.position_at_end(exit);
            }
        }
    }

    pub(crate) fn entry_key(&mut self, entry: PointerValue<'ctx>, key: Ty) -> BasicValueEnum<'ctx> {
        let ptr = self.byte_offset(entry, 8);
        self.b.build_load(self.value_type(key), ptr, "key").must()
    }

    pub(crate) fn entry_value(&mut self, entry: PointerValue<'ctx>, key: Ty, value: Ty) -> BasicValueEnum<'ctx> {
        let offset = self.dict_value_offset(key);
        let ptr = self.byte_offset(entry, offset);
        self.b.build_load(self.value_type(value), ptr, "value").must()
    }

    pub(crate) fn byte_offset(&mut self, ptr: PointerValue<'ctx>, offset: u64) -> PointerValue<'ctx> {
        let offset = self.cx.i64_type().const_int(offset, false);
        // SAFETY (LLVM): the offset stays inside the dict entry the pointer refers to.
        unsafe { self.b.build_in_bounds_gep(self.cx.i8_type(), ptr, &[offset], "").must() }
    }
}
