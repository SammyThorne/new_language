//! The inline allocation fast path (docs/09, phase 2, item 4).
//!
//! Code generation emits every object allocation as a call to `lrt_alloc_raw(size)`, which LLVM
//! knows to be an allocator: the optimisation pipeline deletes allocations whose objects never
//! escape, and forwards stores into fresh objects to later loads. Only once that pipeline has
//! run does [`lower_allocations`] replace each remaining call of a small, constant size with an
//! inline pop from the calling thread's free list for that size. It calls the runtime only when
//! the list is empty (`lrt_alloc_slow` in `lang_rt/src/gc.rs`, which describes the cache).
//!
//! The thread's cache pointer is a thread-local variable defined here, in the program's object,
//! and read with inline assembly marked as having side effects. A task can move to another
//! worker thread whenever it waits, and LLVM assumes a thread-local's address never changes
//! within a function. A plain thread-local access could therefore be reused after a switch, and
//! the task would take objects from another thread's list. The assembly is never merged or moved.

use crate::Must;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::{CallSiteValue, FunctionValue, InstructionOpcode};
use inkwell::{AddressSpace, ThreadLocalMode};

/// The largest size, in 16-byte granules, served from the free lists. Must match
/// `CACHE_GRANULES` in `lang_rt/src/gc.rs`.
const ALLOC_CACHE_GRANULES: u64 = 16;

/// The thread-local variable holding the thread's `AllocCache` pointer, null until the thread's
/// first slow-path allocation.
const CACHE_VARIABLE: &str = "lang_alloc_cache";

/// The granules an object of `size` bytes takes: the size plus the collector's extra byte for
/// interior pointers, rounded up to 16. Must agree with `lrt_alloc_slow`, which asks the
/// collector for `granules * 16 - 1` bytes.
fn granules(size: u64) -> u64 {
    (size + 1).div_ceil(16)
}

/// Replace each call `lrt_alloc_raw(size)` with a constant `size` small enough for the free
/// lists by a call to the inline fast path, then inline it. Returns how many calls were lowered.
pub(crate) fn lower_allocations<'ctx>(cx: &'ctx Context, module: &Module<'ctx>) -> Result<usize, String> {
    let Some(alloc_raw) = module.get_function("lrt_alloc_raw") else { return Ok(0) };
    let mut calls = Vec::new();
    for function in module.get_functions() {
        for block in function.get_basic_blocks() {
            let mut next = block.get_first_instruction();
            while let Some(instruction) = next {
                next = instruction.get_next_instruction();
                if instruction.get_opcode() != InstructionOpcode::Call {
                    continue;
                }
                let Ok(call) = CallSiteValue::try_from(instruction) else { continue };
                if call.get_called_fn_value() != Some(alloc_raw) {
                    continue;
                }
                let size = instruction.get_operand(0).and_then(|operand| operand.value());
                let size = size.and_then(|size| size.into_int_value().get_zero_extended_constant());
                if size.is_some_and(|size| granules(size) <= ALLOC_CACHE_GRANULES) {
                    calls.push(instruction);
                }
            }
        }
    }
    if calls.is_empty() {
        return Ok(0);
    }
    let fast = fast_path(cx, module);
    for call in &calls {
        // The callee is a call's last operand. Both functions have the type `ptr (i64)`, so the
        // call keeps its arguments and its debug location.
        let callee = call.get_num_operands() - 1;
        if !call.set_operand(callee, fast.as_global_value().as_pointer_value()) {
            return Err("could not redirect an allocation call".into());
        }
    }
    Ok(calls.len())
}

/// `ptr @lang.alloc.fast(i64 size)`, always inlined: pop an object of `size` bytes from the
/// thread's free list, or take the slow path.
fn fast_path<'ctx>(cx: &'ctx Context, module: &Module<'ctx>) -> FunctionValue<'ctx> {
    let ptr = cx.ptr_type(AddressSpace::default());
    let i64 = cx.i64_type();

    let cache_variable = module.add_global(ptr, None, CACHE_VARIABLE);
    cache_variable.set_thread_local_mode(Some(ThreadLocalMode::LocalExecTLSModel));
    cache_variable.set_initializer(&ptr.const_null());
    cache_variable.set_linkage(Linkage::External);

    let slow = module.get_function("lrt_alloc_slow").unwrap_or_else(|| {
        let ty = ptr.fn_type(&[i64.into(), i64.into(), ptr.into()], false);
        module.add_function("lrt_alloc_slow", ty, Some(Linkage::External))
    });
    let fast = module.add_function("lang.alloc.fast", ptr.fn_type(&[i64.into()], false), Some(Linkage::Internal));
    let attribute = |name: &str| cx.create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0);
    fast.add_attribute(AttributeLoc::Function, attribute("alwaysinline"));
    fast.add_attribute(AttributeLoc::Function, attribute("nounwind"));

    // The cache pointer, `movq %fs:lang_alloc_cache@tpoff`, and the variable's own address,
    // for the slow path to fill in. Local-exec offsets are fixed at link time, which holds for
    // a variable defined in the executable itself.
    let read_cache = cx.create_inline_asm(
        ptr.fn_type(&[], false),
        format!("movq %fs:{CACHE_VARIABLE}@tpoff, $0"),
        "=r".into(),
        true,
        false,
        None,
        false,
    );
    let cache_address = cx.create_inline_asm(
        ptr.fn_type(&[], false),
        format!("movq %fs:0, $0\n\tleaq {CACHE_VARIABLE}@tpoff($0), $0"),
        "=r".into(),
        true,
        false,
        None,
        false,
    );

    let b = cx.create_builder();
    let entry = cx.append_basic_block(fast, "entry");
    let pop = cx.append_basic_block(fast, "pop");
    let hit = cx.append_basic_block(fast, "hit");
    let miss = cx.append_basic_block(fast, "miss");
    let size = fast.get_nth_param(0).expect("one parameter").into_int_value();

    b.position_at_end(entry);
    let asm_type = ptr.fn_type(&[], false);
    let cache = b.build_indirect_call(asm_type, read_cache, &[], "cache").must();
    let cache = cache.try_as_basic_value().basic().expect("a pointer").into_pointer_value();
    let has_cache = b.build_is_not_null(cache, "").must();
    // `size + 16 >> 4` is `ceil((size + 1) / 16)`, the `granules` above.
    let grains = b
        .build_right_shift(
            b.build_int_add(size, i64.const_int(16, false), "").must(),
            i64.const_int(4, false),
            false,
            "granules",
        )
        .must();
    b.build_conditional_branch(has_cache, pop, miss).must();

    b.position_at_end(pop);
    // SAFETY: `granules` is at most ALLOC_CACHE_GRANULES, within the cache's array of heads.
    let list = unsafe { b.build_gep(ptr, cache, &[grains], "list") }.must();
    let head = b.build_load(ptr, list, "head").must().into_pointer_value();
    let found = b.build_is_not_null(head, "").must();
    b.build_conditional_branch(found, hit, miss).must();

    b.position_at_end(hit);
    let next = b.build_load(ptr, head, "next").must();
    b.build_store(list, next).must();
    // The objects on a list are zeroed except for the link.
    b.build_store(head, ptr.const_null()).must();
    b.build_return(Some(&head)).must();

    b.position_at_end(miss);
    let address = b.build_indirect_call(asm_type, cache_address, &[], "slot").must();
    let address = address.try_as_basic_value().basic().expect("a pointer");
    let object = b.build_call(slow, &[grains.into(), size.into(), address.into()], "object").must();
    let object = object.try_as_basic_value().basic().expect("a pointer");
    b.build_return(Some(&object)).must();

    fast
}

#[cfg(test)]
mod tests {
    use super::granules;

    #[test]
    fn granules_match_the_collector_size_classes() {
        // With the extra byte, 16 bytes need two granules and 15 need one (docs/09, finding 2).
        assert_eq!(granules(8), 1);
        assert_eq!(granules(15), 1);
        assert_eq!(granules(16), 2);
        assert_eq!(granules(24), 2);
        assert_eq!(granules(31), 2);
        assert_eq!(granules(32), 3);
        // The shift in the fast path computes the same.
        for size in 1..300 {
            assert_eq!(granules(size), (size + 16) >> 4);
        }
    }
}
