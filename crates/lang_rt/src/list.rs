//! `list[T]`: a growable array. The element type is known only through the list's descriptor
//! (element size, and whether elements contain references); generated code reads and writes the
//! elements itself, and calls into here for anything that moves memory.
//!
//! The layout must match `list_type` in `lang_codegen`.

use crate::panic::{lrt_panic_index, panic_at};
use crate::{CmpFn, EqFn, TypeDesc, gc, location};

#[repr(C)]
pub struct List {
    pub desc: *const TypeDesc,
    pub len: i64,
    pub cap: i64,
    pub data: *mut u8,
}

unsafe fn elem_size(list: *mut List) -> usize {
    unsafe { (*(*list).desc).elem_size as usize }
}

fn alloc_buffer(desc: *const TypeDesc, cap: usize) -> *mut u8 {
    // SAFETY: descriptors are static constants emitted by the compiler.
    let desc = unsafe { &*desc };
    let bytes = cap * desc.elem_size as usize;
    if desc.elem_has_pointers != 0 { gc::alloc(bytes) } else { gc::alloc_atomic(bytes) }
}

pub fn new(desc: *const TypeDesc, cap: usize) -> *mut List {
    let list = gc::alloc(std::mem::size_of::<List>()) as *mut List;
    // SAFETY: fresh, zeroed allocation of the right size.
    unsafe {
        (*list).desc = desc;
        if cap > 0 {
            (*list).data = alloc_buffer(desc, cap);
            (*list).cap = cap as i64;
        }
    }
    list
}

/// Pointer to element `index`. No bounds check.
///
/// # Safety
/// `list` must be a list and `index` within its capacity.
pub unsafe fn slot(list: *mut List, index: usize) -> *mut u8 {
    unsafe { (*list).data.add(index * elem_size(list)) }
}

unsafe fn reserve(list: *mut List, additional: usize) {
    unsafe {
        let l = &mut *list;
        let needed = l.len as usize + additional;
        if needed <= l.cap as usize {
            return;
        }
        let cap = needed.max(l.cap as usize * 2).max(4);
        let data = alloc_buffer(l.desc, cap);
        if l.len > 0 {
            std::ptr::copy_nonoverlapping(l.data, data, l.len as usize * elem_size(list));
        }
        l.data = data;
        l.cap = cap as i64;
    }
}

/// Grow by one element and return a pointer to the new last slot, for the caller to fill.
///
/// # Safety
/// `list` must be a list.
pub unsafe fn push_slot(list: *mut List) -> *mut u8 {
    unsafe {
        reserve(list, 1);
        let slot = slot(list, (*list).len as usize);
        (*list).len += 1;
        slot
    }
}

/// Remove element `index` (already in range), shifting later elements down.
unsafe fn remove_at(list: *mut List, index: usize, out: *mut u8) {
    unsafe {
        let size = elem_size(list);
        let len = (*list).len as usize;
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(slot(list, index), out, size);
        }
        std::ptr::copy(slot(list, index + 1), slot(list, index), (len - index - 1) * size);
        (*list).len -= 1;
        // Clear the vacated slot so the collector does not keep its old contents alive.
        std::ptr::write_bytes(slot(list, len - 1), 0, size);
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_list_new(desc: *const TypeDesc, cap: i64) -> *mut List {
    new(desc, cap.max(0) as usize)
}

/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_push(list: *mut List) -> *mut u8 {
    unsafe { push_slot(list) }
}

/// Make room at `index` (clamped like Python's `insert`) and return a pointer to the slot.
///
/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_insert(list: *mut List, index: i64) -> *mut u8 {
    unsafe {
        let len = (*list).len;
        let index = if index < 0 { (index + len).max(0) } else { index.min(len) } as usize;
        reserve(list, 1);
        let size = elem_size(list);
        std::ptr::copy(slot(list, index), slot(list, index + 1), (len as usize - index) * size);
        (*list).len += 1;
        slot(list, index)
    }
}

/// Remove the element at `index` (negative counts from the end) and copy it to `out`.
///
/// # Safety
/// `list` must be a list, `out` must have room for one element, and the location must be UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_pop(list: *mut List, index: i64, out: *mut u8, loc: *const u8, loc_len: i64) {
    unsafe {
        let len = (*list).len;
        if len == 0 {
            panic_at("pop from an empty list", location(loc, loc_len));
        }
        let adjusted = if index < 0 { index + len } else { index };
        if adjusted < 0 || adjusted >= len {
            lrt_panic_index(index, len, loc, loc_len);
        }
        remove_at(list, adjusted as usize, out);
    }
}

/// `list[start:stop]`, a new list.
///
/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_slice(
    list: *mut List,
    start: i64,
    stop: i64,
    has_start: i64,
    has_stop: i64,
) -> *mut List {
    unsafe {
        let len = (*list).len;
        let bound = |value: i64, present: i64, default: i64| {
            if present == 0 {
                default
            } else if value < 0 {
                (value + len).max(0)
            } else {
                value.min(len)
            }
        };
        let start = bound(start, has_start, 0);
        let stop = bound(stop, has_stop, len).max(start);
        let count = (stop - start) as usize;
        let out = new((*list).desc, count);
        if count > 0 {
            std::ptr::copy_nonoverlapping(slot(list, start as usize), (*out).data, count * elem_size(list));
        }
        (*out).len = count as i64;
        out
    }
}

/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_copy(list: *mut List) -> *mut List {
    unsafe { lrt_list_slice(list, 0, 0, 0, 0) }
}

/// Append every element of `other` (which may be the same list).
///
/// # Safety
/// Both must be lists of the same type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_extend(list: *mut List, other: *mut List) {
    unsafe {
        let count = (*other).len as usize;
        reserve(list, count);
        if count > 0 {
            let size = elem_size(list);
            std::ptr::copy(slot(other, 0), slot(list, (*list).len as usize), count * size);
        }
        (*list).len += count as i64;
    }
}

/// `a + b`, a new list.
///
/// # Safety
/// Both must be lists of the same type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_concat(a: *mut List, b: *mut List) -> *mut List {
    unsafe {
        let out = new((*a).desc, ((*a).len + (*b).len) as usize);
        lrt_list_extend(out, a);
        lrt_list_extend(out, b);
        out
    }
}

/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_clear(list: *mut List) {
    unsafe {
        (*list).len = 0;
        (*list).cap = 0;
        (*list).data = std::ptr::null_mut();
    }
}

/// # Safety
/// `list` must be a list.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_reverse(list: *mut List) {
    unsafe {
        let len = (*list).len as usize;
        let size = elem_size(list);
        for i in 0..len / 2 {
            std::ptr::swap_nonoverlapping(slot(list, i), slot(list, len - 1 - i), size);
        }
    }
}

/// Index of the first element equal to `*value`, or -1.
///
/// # Safety
/// `list` must be a list, `value` must point to an element-typed value, and `eq` must compare
/// two elements.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_index_of(list: *mut List, value: *const u8, eq: EqFn) -> i64 {
    unsafe {
        for i in 0..(*list).len as usize {
            if eq(slot(list, i), value) != 0 {
                return i as i64;
            }
        }
    }
    -1
}

/// Remove the first element equal to `*value`; panic if there is none.
///
/// # Safety
/// As for `lrt_list_index_of`; the location must be UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_remove_value(
    list: *mut List,
    value: *const u8,
    eq: EqFn,
    loc: *const u8,
    loc_len: i64,
) {
    unsafe {
        let index = lrt_list_index_of(list, value, eq);
        if index < 0 {
            panic_at("remove(): the value is not in the list", location(loc, loc_len));
        }
        remove_at(list, index as usize, std::ptr::null_mut());
    }
}

/// Stable sort with a three-way comparison of two elements.
///
/// # Safety
/// `list` must be a list and `cmp` must compare two of its elements.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_sort(list: *mut List, cmp: CmpFn) {
    unsafe {
        let len = (*list).len as usize;
        if len < 2 {
            return;
        }
        // Sort indices (plain numbers, so nothing collected lives in Rust memory), then copy the
        // elements into a new buffer in that order. Nothing is allocated while sorting, and the
        // old buffer stays reachable through the list until it is replaced.
        let mut order: Vec<usize> = (0..len).collect();
        order.sort_by(|&a, &b| cmp(slot(list, a), slot(list, b)).cmp(&0));
        let size = elem_size(list);
        let data = alloc_buffer((*list).desc, (*list).cap as usize);
        for (to, &from) in order.iter().enumerate() {
            std::ptr::copy_nonoverlapping(slot(list, from), data.add(to * size), size);
        }
        (*list).data = data;
    }
}

/// Sort a list stably by precomputed keys (`keys[i]` belongs to `list[i]`), or by the elements
/// themselves when `keys` is null. With `reverse`, the order is descending but elements with equal
/// keys keep their original order, as in Python.
///
/// # Safety
/// `list` must be a list; `keys`, if not null, a list of the same length; `cmp` must compare two
/// keys (or two elements).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_sort_by(list: *mut List, keys: *mut List, cmp: CmpFn, reverse: i64) {
    unsafe {
        let len = (*list).len as usize;
        if len < 2 {
            return;
        }
        let by = if keys.is_null() { list } else { keys };
        let mut order: Vec<usize> = (0..len).collect();
        if reverse != 0 {
            order.sort_by(|&a, &b| cmp(slot(by, b), slot(by, a)).cmp(&0));
        } else {
            order.sort_by(|&a, &b| cmp(slot(by, a), slot(by, b)).cmp(&0));
        }
        let size = elem_size(list);
        let data = alloc_buffer((*list).desc, (*list).cap as usize);
        for (to, &from) in order.iter().enumerate() {
            std::ptr::copy_nonoverlapping(slot(list, from), data.add(to * size), size);
        }
        (*list).data = data;
    }
}

/// # Safety
/// Both must be lists of the same type and `eq` must compare two elements.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_list_eq(a: *mut List, b: *mut List, eq: EqFn) -> i64 {
    unsafe {
        if (*a).len != (*b).len {
            return 0;
        }
        for i in 0..(*a).len as usize {
            if eq(slot(a, i), slot(b, i)) == 0 {
                return 0;
            }
        }
    }
    1
}
