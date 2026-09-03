//! Runs Klaus Dormann's 6502 functional test against the CPU core.
//!
//! This is the gold-standard correctness check for a 6502 implementation: it
//! exercises every official opcode, addressing mode, flag interaction, and
//! decimal-mode arithmetic, trapping (branching to itself) the instant anything
//! is wrong.
//!
//! The binary is GPL, so it is NOT vendored. Fetch it once:
//!
//! ```text
//! curl -L -o test-data/6502_functional_test.bin \
//!   https://github.com/Klaus2m5/6502_65C02_functional_tests/raw/master/bin_files/6502_functional_test.bin
//! ```
//!
//! If it is absent, this test skips (prints a note) rather than failing.

use mos6502::{Bus, Cpu};

/// Full 64 KB image is loaded verbatim at $0000.
const LOAD_ADDR: u16 = 0x0000;
/// The test's entry point.
const ENTRY: u16 = 0x0400;
/// Address of the success trap in this particular build (decimal enabled).
const SUCCESS: u16 = 0x3469;

struct Ram(Vec<u8>);
impl Bus for Ram {
    fn read(&mut self, addr: u16) -> u8 {
        self.0[addr as usize]
    }
    fn write(&mut self, addr: u16, val: u8) {
        self.0[addr as usize] = val;
    }
}

#[test]
fn klaus_functional_test() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/6502_functional_test.bin");
    let image = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("skipping klaus_functional_test: {path} not found (see file header to fetch it)");
            return;
        }
    };
    assert_eq!(image.len(), 0x10000, "expected a full 64 KB image");

    let mut ram = Ram(vec![0; 0x10000]);
    ram.0[LOAD_ADDR as usize..].copy_from_slice(&image);

    let mut cpu = Cpu::new();
    cpu.pc = ENTRY;

    let mut last_pc = 0xFFFFu16;
    for _ in 0..300_000_000u64 {
        let pc = cpu.pc;
        cpu.step(&mut ram);
        // A branch/jump to itself leaves PC unchanged: that's a trap.
        if cpu.pc == pc {
            assert_eq!(
                cpu.pc, SUCCESS,
                "functional test trapped at ${:04X} (a failing sub-test), not the success trap ${:04X}",
                cpu.pc, SUCCESS
            );
            return;
        }
        last_pc = pc;
    }
    panic!("functional test did not terminate; last PC ${last_pc:04X}");
}
