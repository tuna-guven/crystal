//! Zeroizing global allocator: a `GlobalAlloc` wrapper that zeroes every
//! block before freeing it and on realloc, so stale plaintext copies never
//! linger in freed heap.
//!
//! This is the only crate in the workspace allowed to use `unsafe`.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]
