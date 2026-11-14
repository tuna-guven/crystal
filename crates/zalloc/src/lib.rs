//! Zeroizing global allocator: a `GlobalAlloc` wrapper that zeroes every
//! block before freeing it, including the old block of every realloc, so
//! stale plaintext copies never linger in freed heap
//! (docs/design/dependencies.md, docs/design/leakage.md).
//!
//! This is the only crate in the workspace allowed to use `unsafe`.
//!
//! - `dealloc` wipes the whole block with volatile writes and a compiler
//!   fence, then hands it to the wrapped allocator. Plain writes would not
//!   do: the compiler knows `free` and may remove stores to memory that is
//!   about to be freed.
//! - `realloc` is not overridden. `GlobalAlloc`'s provided `realloc`
//!   allocates a new block, copies, and calls `self.dealloc` on the old
//!   one, so the old block is wiped. Forwarding to the wrapped allocator's
//!   `realloc` would let it move the data and free the old block unwiped.
//!   The cost is that blocks never grow in place.
//! - `alloc` and `alloc_zeroed` forward unchanged.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::sync::atomic::{Ordering, compiler_fence};

/// The allocator `A`, with every freed block zeroed first. Crystal installs
/// `Zeroizing(std::alloc::System)` as its global allocator (`src/sys.rs`).
pub struct Zeroizing<A>(pub A);

/// Writes zeros over `len` bytes at `ptr`: single bytes up to the first
/// word boundary, whole words, then the remaining bytes. Every write is
/// volatile, and the fence keeps them from being moved past the free that
/// follows.
///
/// # Safety
///
/// `ptr` must be valid for writes of `len` bytes.
unsafe fn wipe(ptr: *mut u8, len: usize) {
    // `align_offset` may answer usize::MAX ("cannot align"); then every
    // byte is written singly.
    let head = ptr.align_offset(align_of::<usize>()).min(len);
    let mut at = 0;
    while at < head {
        // SAFETY: at < head <= len, so the byte is inside the block the
        // caller vouches for.
        unsafe { ptr.add(at).write_volatile(0) };
        at += 1;
    }
    while len - at >= size_of::<usize>() {
        // SAFETY: after the head, ptr + at is aligned for usize (when the
        // head stopped early at len, this loop does not run), and the word
        // ends at most at len.
        unsafe { ptr.add(at).cast::<usize>().write_volatile(0) };
        at += size_of::<usize>();
    }
    while at < len {
        // SAFETY: at < len.
        unsafe { ptr.add(at).write_volatile(0) };
        at += 1;
    }
    compiler_fence(Ordering::SeqCst);
}

// SAFETY: `alloc` and `alloc_zeroed` forward to `A` unchanged, so `A`'s
// guarantees hold for what they return. `dealloc` only writes zeros into
// the block, which the caller still owns, before passing the same pointer
// and layout to `A`. `realloc` is GlobalAlloc's provided method, built on
// these three.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Zeroizing<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller meets `alloc`'s contract; it is passed on as is.
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller guarantees `ptr` is a live block of this
        // allocator with `layout`, so it is valid for `layout.size()` bytes
        // of writes.
        unsafe { wipe(ptr, layout.size()) };
        // SAFETY: the same block and layout the caller passed, still live.
        unsafe { self.0.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use core::cell::Cell;
    use std::alloc::System;
    use std::vec::Vec;

    /// The system allocator, but its `dealloc` first checks that every
    /// byte of the block is zero, and counts the blocks it checked. Wrapped
    /// in `Zeroizing`, it sees each block right after the wipe and before
    /// it is freed, so nothing reads freed memory.
    #[derive(Default)]
    struct MustBeWiped {
        checked: Cell<usize>,
    }

    // SAFETY: forwards to `System`; `dealloc` only reads the block, which
    // is still live, before freeing it.
    unsafe impl GlobalAlloc for MustBeWiped {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller's contract, passed on.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: a live block of `layout.size()` bytes, initialized:
            // every test writes all of it before freeing, and `wipe` has
            // just written zeros.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, layout.size()) };
            assert!(bytes.iter().all(|&b| b == 0), "freed unwiped");
            self.checked.set(self.checked.get() + 1);
            // SAFETY: the same block and layout.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    const SIZES: [usize; 9] = [1, 7, 8, 9, 15, 63, 64, 4096, 100_003];
    const ALIGNS: [usize; 4] = [1, 8, 16, 64];

    /// A block of `layout` filled with 0xAA.
    fn filled(a: &Zeroizing<MustBeWiped>, layout: Layout) -> *mut u8 {
        // SAFETY: every layout here has a non-zero size.
        let ptr = unsafe { a.alloc(layout) };
        assert!(!ptr.is_null());
        // SAFETY: a fresh block of `layout.size()` bytes.
        unsafe { ptr.write_bytes(0xAA, layout.size()) };
        ptr
    }

    #[test]
    fn freed_blocks_are_wiped() {
        let a = Zeroizing(MustBeWiped::default());
        for size in SIZES {
            for align in ALIGNS {
                let layout = Layout::from_size_align(size, align).unwrap();
                let ptr = filled(&a, layout);
                // SAFETY: the block just allocated with `layout`.
                unsafe { a.dealloc(ptr, layout) };
            }
        }
        assert_eq!(a.0.checked.get(), SIZES.len() * ALIGNS.len());
    }

    /// Growing and shrinking: the old block is wiped before it is freed,
    /// and the new one holds the old bytes (up to the smaller size).
    #[test]
    fn realloc_wipes_the_old_block() {
        let a = Zeroizing(MustBeWiped::default());
        for (from, to) in [(8, 4096), (100, 7), (63, 64), (4096, 100_003)] {
            let layout = Layout::from_size_align(from, 8).unwrap();
            let ptr = filled(&a, layout);
            let before = a.0.checked.get();
            // SAFETY: a live block of `layout`; `to` is non-zero and small.
            let grown = unsafe { a.realloc(ptr, layout, to) };
            assert!(!grown.is_null());
            assert_eq!(
                a.0.checked.get(),
                before + 1,
                "the old block went through dealloc"
            );
            let kept = from.min(to);
            // SAFETY: the new block holds `kept` copied bytes.
            let copied = unsafe { core::slice::from_raw_parts(grown, kept) };
            assert!(copied.iter().all(|&b| b == 0xAA));
            // Fill the rest, so the final check sees only the wipe.
            // SAFETY: the new block has `to` bytes.
            unsafe { grown.add(kept).write_bytes(0xAA, to - kept) };
            let new_layout = Layout::from_size_align(to, 8).unwrap();
            // SAFETY: the block realloc returned, with its layout.
            unsafe { a.dealloc(grown, new_layout) };
        }
    }

    /// `wipe` at every start offset and length around a word: exactly the
    /// range is zeroed, and the bytes on either side are untouched.
    #[test]
    fn wipe_covers_exactly_its_range() {
        for start in 0..17 {
            for len in 0..40 {
                let mut buf: Vec<u8> = std::vec![0xAA; 64];
                // SAFETY: start + len <= 56 < 64, inside `buf`.
                unsafe { wipe(buf.as_mut_ptr().add(start), len) };
                for (i, &b) in buf.iter().enumerate() {
                    let inside = (start..start + len).contains(&i);
                    assert_eq!(b == 0, inside, "start {start} len {len} byte {i}");
                }
            }
        }
    }
}
