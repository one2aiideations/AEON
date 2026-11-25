//! A holder for state that is only ever used from ONE execution context.
//!
//! Many modules in this crate keep mutable state in `UnsafeCell`s behind `&self` methods, with
//! no locking, because on a single core with one control loop that is simple and fast. Such
//! types are deliberately NOT `Sync`: letting two threads (or an interrupt handler and the main
//! loop) call them at once would be a data race. A `static` has to be `Sync`, so bare-metal code
//! that needs one wraps it here, and the unsafe constructor is where the integrator takes
//! responsibility for the single-context rule.

/// A value that the integrator promises is only accessed from a single execution context.
pub struct SingleContext<T>(T);

// SAFETY: `SingleContext::new` is `unsafe`, and its contract is that the value is only ever
// accessed from one execution context (one core, with no interrupt handler touching it). Under
// that contract there is never concurrent access, so sharing the wrapper in a `static` is sound.
unsafe impl<T> Sync for SingleContext<T> {}

impl<T> SingleContext<T> {
    /// Wraps `value`.
    ///
    /// # Safety
    /// The caller guarantees that the value, and anything reachable through it, is only ever
    /// used from one execution context, for the whole life of the wrapper.
    pub const unsafe fn new(value: T) -> Self {
        Self(value)
    }

    /// Access to the wrapped value.
    pub fn get(&self) -> &T {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn wraps_non_sync_state_so_it_can_live_in_a_static() {
        // `Cell` is not Sync; the wrapper makes a static possible once the caller vouches for it.
        // SAFETY: this test only touches the value from the test thread.
        static HOLDER: SingleContext<Cell<u32>> = unsafe { SingleContext::new(Cell::new(7)) };
        assert_eq!(HOLDER.get().get(), 7);
        HOLDER.get().set(9);
        assert_eq!(HOLDER.get().get(), 9);
    }
}
