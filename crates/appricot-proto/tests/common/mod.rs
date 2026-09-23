//! Support shared by this crate's integration tests: an allocator that counts.
//!
//! A test binary installs [`Counting`] as its global allocator and measures a call with
//! [`peak_during`]. The counters are per thread, so tests running in parallel threads of the
//! same binary do not disturb each other's numbers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Bytes this thread holds right now through the counting allocator.
    static HELD: Cell<usize> = const { Cell::new(0) };
    /// The most `HELD` has been since the last [`peak_during`] started.
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

/// The system allocator, counting what each thread holds and the most it held.
#[derive(Debug)]
pub struct Counting;

fn grew(bytes: usize) {
    let _ = HELD.try_with(|held| {
        let now = held.get().saturating_add(bytes);
        held.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

fn shrank(bytes: usize) {
    let _ = HELD.try_with(|held| held.set(held.get().saturating_sub(bytes)));
}

// A `GlobalAlloc` impl is unsafe by definition, and a test cannot count allocations without
// one. Every method forwards its arguments unchanged to `System`, so this allocator keeps
// exactly the contract `System` keeps. The counters are thread-locals with const initialisers
// and no destructor, so touching them never allocates and never re-enters this allocator.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout goes to System unchanged.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout goes to System unchanged.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator, which is System, with this layout.
        unsafe { System.dealloc(ptr, layout) };
        shrank(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from System with this layout; the caller vouches for new_size.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            shrank(layout.size());
            grew(new_size);
        }
        moved
    }
}

/// Runs `f` and returns its result with the most bytes this thread held while `f` ran, over
/// what it held before `f` started.
pub fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = HELD.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    let out = f();
    let peak = PEAK.with(Cell::get);
    (out, peak.saturating_sub(base))
}
