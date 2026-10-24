//! The stack headroom the translator's `#[recursive]` functions need.

/// The most one step of a translator recursion cycle uses, which a debug build
/// takes up to about 140 KiB of.
#[cfg(feature = "std")]
const HEADROOM: usize = 256 * 1024;

/// Raises `recursive`'s process-wide minimum to [`HEADROOM`], never lowering
/// it.
pub(crate) fn ensure() {
    #[cfg(feature = "std")]
    if recursive::get_minimum_stack_size() < HEADROOM {
        recursive::set_minimum_stack_size(HEADROOM);
    }
}
