//! Windows capture APO: native COM boundary and bounded transport to Clearwave.
//! Model inference stays in the separate desktop-owned process, never audiodg.
#![cfg_attr(not(windows), allow(dead_code))]
#![deny(unsafe_code)]
mod bridge;
#[cfg(windows)]
#[allow(unsafe_code)]
mod native;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod pipe;
pub mod protocol;
