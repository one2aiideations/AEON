//! MOULI tensor primitives: element types, row-major layouts, owned
//! tensors, borrowed views with slicing, and a bump arena.
//!
//! Ported from the retired C core (`mouli_tensor`, `mouli_tensor_view`,
//! `mouli_runtime_memory`). All size arithmetic is checked.
#![forbid(unsafe_code)]

pub mod arena;
pub mod layout;
pub mod view;

pub use arena::{Arena, ArenaError};
pub use layout::{DType, Layout, TensorError, MAX_RANK};
pub use view::{Tensor, TensorView};
