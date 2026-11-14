//! Safe glue for low-level pieces, such as installing the zeroizing global
//! allocator. No `unsafe` here; that lives in its own crate.

/// Every allocation in Crystal goes through the zeroizing allocator: a
/// freed block, and the old block of every realloc, is wiped before the
/// system allocator gets it back (docs/design/leakage.md, "Stale heap
/// copies after realloc").
#[global_allocator]
static ALLOCATOR: zalloc::Zeroizing<std::alloc::System> = zalloc::Zeroizing(std::alloc::System);
