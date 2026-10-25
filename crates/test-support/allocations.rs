// Test-only allocation traffic, scoped to the calling test thread.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountAllocations;

fn record(bytes: usize) {
    let _ = ALLOCATED.try_with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current + bytes));
        }
    });
}

unsafe impl GlobalAlloc for CountAllocations {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountAllocations = CountAllocations;

pub fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATED.with(|count| count.set(Some(0)));
    let result = f();
    let bytes = ALLOCATED.with(|count| count.replace(None).unwrap());
    (result, bytes)
}
