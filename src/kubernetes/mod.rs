//! Kubernetes configuration lane (implemented by later tickets).
//!
//! Submodule layout is predeclared here by LATCH-2 / F02 so that the CRD,
//! watch source, and status publication code each lands in its own file
//! without editing the crate root.

pub mod crd;
pub mod source;
pub mod status;
