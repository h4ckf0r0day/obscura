#![deny(unused)]
#![deny(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(test, deny(warnings))]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![doc = include_str!("../README.md")]

#[cfg(feature = "emulation")]
pub mod emulate;
mod rand;
pub mod tower;

#[cfg(feature = "emulation")]
pub use self::emulate::{Emulation, Platform, Profile};
