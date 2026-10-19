use mmx_consensus::Variant;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static LARGEST: Cell<Option<usize>> = const { Cell::new(None) };
}
struct ObservedAllocator;
fn observe(size: usize) {
    let _ = LARGEST.try_with(|n| {
        if let Some(old) = n.get() {
            n.set(Some(old.max(size)));
        }
    });
}
unsafe impl GlobalAlloc for ObservedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        observe(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: ObservedAllocator = ObservedAllocator;

#[test]
fn truncated_container_headers_do_not_reserve_the_declared_element_storage() {
    // Both declared containers fit the aggregate budget, but their payloads
    // are absent. Neither may allocate millions of slots before reading them.
    for (code, count) in [(&[12u16, 3][..], 2_000_000u32), (&[24][..], 1_000_000)] {
        let mut bytes = (code.len() as u16).to_le_bytes().to_vec();
        for word in code {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend(count.to_le_bytes());
        LARGEST.set(Some(0));
        let error = Variant::decode(&bytes).unwrap_err();
        let largest = LARGEST.replace(None).unwrap();
        assert!(error.to_string().contains("truncated"), "{error}");
        assert!(
            largest <= 16 * 1024,
            "reserved {largest} bytes from a length header"
        );
    }
}
