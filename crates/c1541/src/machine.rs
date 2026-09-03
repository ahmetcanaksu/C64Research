//! A bootable 1541 disk drive: the 6502 CPU wired to the drive's RAM, ROM, and
//! two 6522 VIAs over the real memory map.
//!
//! Memory map (the drive CPU's 64 KB space):
//!
//! ```text
//!   $0000-$07FF   2 KB RAM
//!   $1800-$1BFF   VIA1  (serial IEC bus)      — mirrored every 16 bytes
//!   $1C00-$1FFF   VIA2  (disk controller)     — mirrored every 16 bytes
//!   $C000-$FFFF   16 KB DOS ROM (C1541.rom)
//!   (everything else reads as open bus / 0)
//! ```
//!
//! The ROM is embedded with `include_bytes!`, so a built `c1541` is fully
//! self-contained — no file to ship, and ready to run from flash on an MCU.

use mos6502::{Bus, Cpu};
use via6522::Via;

/// The embedded 1541 DOS ROM (16 KB), mapped at $C000.
pub const ROM: &[u8; 0x4000] = include_bytes!("../../../roms/C1541.rom");

const RAM_SIZE: usize = 0x0800;

/// Everything the CPU talks to: RAM, ROM, and the two VIAs.
pub struct Board {
    pub ram: [u8; RAM_SIZE],
    pub rom: &'static [u8; 0x4000],
    pub via1: Via,
    pub via2: Via,
}

impl Default for Board {
    fn default() -> Self {
        Board {
            ram: [0; RAM_SIZE],
            rom: ROM,
            via1: Via::new(),
            via2: Via::new(),
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x07FF => self.ram[addr as usize],
            0x1800..=0x1BFF => self.via1.read(addr as u8 & 0x0F),
            0x1C00..=0x1FFF => self.via2.read(addr as u8 & 0x0F),
            0xC000..=0xFFFF => self.rom[(addr - 0xC000) as usize],
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x07FF => self.ram[addr as usize] = val,
            0x1800..=0x1BFF => self.via1.write(addr as u8 & 0x0F, val),
            0x1C00..=0x1FFF => self.via2.write(addr as u8 & 0x0F, val),
            _ => {} // ROM and unmapped space ignore writes
        }
    }
}

/// The whole drive: CPU + board. Kept as separate fields so the CPU can borrow
/// the board as its bus without aliasing.
pub struct Machine {
    pub cpu: Cpu,
    pub board: Board,
}

impl Default for Machine {
    fn default() -> Self {
        let mut m = Machine { cpu: Cpu::new(), board: Board::default() };
        m.reset();
        m
    }
}

impl Machine {
    /// Build and reset a fresh drive.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pull RESET: load PC from the reset vector.
    pub fn reset(&mut self) {
        self.cpu.reset(&mut self.board);
    }

    /// Execute one instruction, advance both VIA timers by the cycles it took,
    /// and service an IRQ if either VIA is asserting one. Returns those cycles.
    pub fn step(&mut self) -> u8 {
        let cycles = self.cpu.step(&mut self.board);
        self.board.via1.tick(cycles as u32);
        self.board.via2.tick(cycles as u32);
        if self.board.via1.irq_asserted() || self.board.via2.irq_asserted() {
            self.cpu.irq(&mut self.board);
        }
        cycles
    }

    /// Run for at least `cycles` clocks (finishing the instruction that crosses
    /// the boundary). Returns the number of instructions executed.
    pub fn run_cycles(&mut self, cycles: u64) -> u64 {
        let mut spent = 0u64;
        let mut instrs = 0u64;
        while spent < cycles {
            spent += self.step() as u64;
            instrs += 1;
        }
        instrs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_is_embedded_and_vectors_are_sane() {
        assert_eq!(ROM.len(), 0x4000);
        let m = Machine::new();
        // Reset vector points at the canonical 1541 reset entry $EAA0.
        assert_eq!(m.cpu.pc, 0xEAA0);
    }

    /// The headline test: the real DOS ROM boots all the way to its main idle
    /// loop. Getting here means the CPU passed the zero-page RAM test and the
    /// ROM checksum, the VIAs came up, interrupts were enabled (`CLI`), and the
    /// drive is now scanning its job queue waiting for work — a healthy,
    /// powered-on 1541.
    ///
    /// Idle-loop head: $EC00 (`LDA $7C` right after the `CLI` at $EBFF).
    const IDLE_LOOP_HEAD: u16 = 0xEC00;
    /// The RAM-fault handler; reaching it would mean boot failed.
    const RAM_FAULT: u16 = 0xEA6E;

    #[test]
    fn boots_into_dos_idle_loop() {
        let mut m = Machine::new();
        let mut idle_visits = 0u32;
        // Generous budget: boot (RAM test + checksum) is a few hundred k cycles.
        for _ in 0..5_000_000u64 {
            m.step();
            match m.cpu.pc {
                RAM_FAULT => panic!("drive branched to the RAM-fault handler at $EA6E"),
                IDLE_LOOP_HEAD => {
                    idle_visits += 1;
                    if idle_visits >= 3 {
                        return; // reached and is looping in the idle loop: booted
                    }
                }
                _ => {}
            }
        }
        panic!(
            "drive never reached the DOS idle loop; stuck near ${:04X}",
            m.cpu.pc
        );
    }
}
