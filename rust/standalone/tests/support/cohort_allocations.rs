//! Thread-local allocation observation; only this test binary installs the hook.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counted;
#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn count() {
    if ACTIVE.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNT.try_with(|value| value.set(value.get() + 1));
    }
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
pub(super) fn begin() {
    COUNT.set(0);
    ACTIVE.set(true);
}
pub(super) fn end() -> usize {
    ACTIVE.set(false);
    COUNT.get()
}
