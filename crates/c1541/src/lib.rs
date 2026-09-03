//! Hand-ported 1541 disk-drive DOS routines, translated from `C1541.rom`.
//!
//! Each routine keeps the original 6502 listing next to the Rust so the
//! translation can be audited byte-for-byte against the ROM. This is the
//! "understand → port" half of the disassemble → understand → port pipeline.

#![no_std]

pub mod machine;
pub mod ram_test;

pub use machine::Machine;
