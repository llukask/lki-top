//! The four btop boxes. The graph boxes are plain state the root view
//! owns and renders; the process box is a view of its own, because it
//! holds the keyboard focus.

pub mod cpu;
pub mod mem;
pub mod net;
pub mod proc;
