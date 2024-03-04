use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;

#[global_allocator]
static ALLOC: CustomAlloc = CustomAlloc;

struct CustomAlloc;

// This is where one would do something interesting.
// Alternatively, use an existing type and impl from a library crate like `tikv-jemallocator`
unsafe impl GlobalAlloc for CustomAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        System.realloc(ptr, layout, new_size)
    }
}

fn main() {
    match apollo_router::main() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
