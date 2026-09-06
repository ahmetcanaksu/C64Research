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

/// The drive's device address (bits 5-6 of VIA1 port B are the jumpers).
/// Device 8 leaves both jumpers grounded — those bits read 0.
const DEVICE_8_JUMPERS: u8 = 0x00;

impl Board {
    /// Sample the serial bus into VIA1's inputs (`$1800`), before the CPU reads
    /// them.
    ///
    /// Port B bit assignments: PB0 DATA-in, PB2 CLK-in, PB7 ATN-in, PB5-6 the
    /// device-address jumpers. DATA and CLK read the line level directly (1 =
    /// released/high); **ATN is inverted** on the 1541, so PB7 (and CA1) read 1
    /// while ATN is asserted. The DOS wires ATN to CA1 with PCR=$01 (rising
    /// edge), so the drive takes an interrupt the instant the C64 asserts ATN.
    pub fn sync_bus_in(&mut self, bus: &iec::Bus) {
        let mut pb = 0xFF;
        if !bus.data() {
            pb &= !0x01; // DATA pulled low -> PB0 = 0
        }
        if !bus.clk() {
            pb &= !0x04; // CLK pulled low -> PB2 = 0
        }
        if bus.atn() {
            pb &= !0x80; // ATN released -> inverted PB7 = 0
        }
        pb = (pb & !0x60) | DEVICE_8_JUMPERS;
        self.via1.pb_in = pb;
        // CA1 = inverted ATN: high while ATN is asserted (bus low).
        self.via1.set_ca1(!bus.atn());
    }

    /// Push VIA1's outputs onto the bus after the CPU has run.
    ///
    /// PB1/PB3 are the DOS's DATA/CLK outputs (1 = pull the line low), and PB4
    /// is ATNA. The attention acknowledge is **hardware**: a gate pulls DATA low
    /// whenever ATNA disagrees with the ATN line, so a drive answers attention
    /// in nanoseconds — before its CPU runs — wired-OR with the DOS's own DATA
    /// output.
    pub fn sync_bus_out(&self, bus: &mut iec::Bus, slot: usize) {
        let pb = self.via1.port_b();
        let mut pulls = 0;
        if pb & 0x02 != 0 {
            pulls |= iec::line::DATA;
        }
        if pb & 0x08 != 0 {
            pulls |= iec::line::CLK;
        }
        let atna = pb & 0x10 != 0;
        let atn_asserted = !bus.atn();
        if atna != atn_asserted {
            pulls |= iec::line::DATA; // hardware ATN acknowledge
        }
        bus.set_pulls(slot, pulls);
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

    /// Execute one instruction with the drive's VIA1 wired to a shared serial
    /// `bus` at device slot `slot`: sample the bus in, step, drive the bus out.
    pub fn step_on_bus(&mut self, bus: &mut iec::Bus, slot: usize) -> u8 {
        self.board.sync_bus_in(bus);
        let cycles = self.step();
        self.board.sync_bus_out(bus, slot);
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

    /// 1a milestone: the drive boots on a shared serial bus and answers ATN. It
    /// releases DATA at idle, and pulls DATA low the moment the controller
    /// asserts attention — the hardware acknowledge plus the DOS taking its CA1
    /// interrupt.
    #[test]
    fn drive_answers_attention_on_the_bus() {
        let mut drive = Machine::new();
        let mut bus = iec::Bus::new();

        // Boot to the idle loop, clocked on the bus.
        let mut booted = false;
        for _ in 0..3_000_000u64 {
            drive.step_on_bus(&mut bus, 1);
            if drive.cpu.pc == IDLE_LOOP_HEAD {
                booted = true;
                break;
            }
        }
        assert!(booted, "drive did not boot on the bus");

        // Idle with ATN released: the drive must not be holding DATA down.
        for _ in 0..5_000 {
            drive.step_on_bus(&mut bus, 1);
        }
        assert!(bus.data(), "at idle the drive should release DATA");

        // The controller asserts ATN — the drive must pull DATA low to answer.
        bus.pull(iec::CONTROLLER, iec::Line::Atn, true);
        let mut answered = false;
        for _ in 0..20_000 {
            drive.step_on_bus(&mut bus, 1);
            if !bus.data() {
                answered = true;
                break;
            }
        }
        assert!(answered, "the drive must pull DATA low to acknowledge ATN");
    }

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
