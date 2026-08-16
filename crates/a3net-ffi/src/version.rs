//! FFI version constant.
//!
//! Bumped by hand when the C-ABI surface changes in a
//! non-additive way. The C header mirrors this value as
//! `ADNET_FFI_VERSION` so embedders can refuse to load a
//! mismatched library.

/// A3Net FFI major version. Bumped when the C-ABI surface
/// changes in a way that requires embedder changes (renamed
/// functions, new mandatory arguments, removed fields).
pub const ADNET_FFI_VERSION: u32 = 1;
