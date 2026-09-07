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

use crate::disk::{Controller, Disk};
use mos6502::{Bus, Cpu};
use via6522::Via;

/// The embedded 1541 DOS ROM (16 KB), mapped at $C000.
pub const ROM: &[u8; 0x4000] = include_bytes!("../../../roms/C1541.rom");

const RAM_SIZE: usize = 0x0800;

/// Everything the CPU talks to: RAM, ROM, the two VIAs, and the disk mechanism
/// wired to VIA2.
pub struct Board {
    pub ram: [u8; RAM_SIZE],
    pub rom: &'static [u8; 0x4000],
    pub via1: Via,
    pub via2: Via,
    pub disk: Controller,
}

impl Default for Board {
    fn default() -> Self {
        Board {
            ram: [0; RAM_SIZE],
            rom: ROM,
            via1: Via::new(),
            via2: Via::new(),
            disk: Controller::default(),
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
    /// device-address jumpers.
    ///
    /// **All three serial inputs are inverted** — they reach the VIA through
    /// inverting buffers, so a `1` means the line is *pulled low*, not high.
    /// The DOS's own bit-receive loop proves it for DATA:
    ///
    /// ```text
    ///   EA0B:  LDA $1800
    ///   EA0E:  EOR #$01     ; flip PB0 ...
    ///   EA10:  LSR A        ; ... into the carry
    ///   EA11:  AND #$02     ; (and isolate PB2, the clock)
    ///   EA13:  BNE $EA0B    ; wait for the clock edge
    ///   EA18:  ROR $85      ; the *flipped* PB0 is the data bit
    /// ```
    ///
    /// A released DATA line means a one bit, and the DOS shifts in `NOT PB0` —
    /// so `PB0 = 0` is a released line. The same loop pins CLK down: a listener
    /// samples on the clock's *rising* edge, and this one leaves the wait when
    /// PB2 is `0`, so `PB2 = 0` is CLK high.
    ///
    /// Model these the obvious way round instead and everything still boots and
    /// still acknowledges attention — it just deadlocks halfway through the
    /// first byte, with each side waiting for a line the other thinks it already
    /// released.
    ///
    /// The DOS wires ATN to CA1 with PCR=$01 (rising edge), so the drive takes
    /// an interrupt the instant the C64 asserts ATN.
    pub fn sync_bus_in(&mut self, bus: &iec::Bus) {
        let mut pb = 0xFF;
        if bus.data() {
            pb &= !0x01; // DATA released -> inverted PB0 = 0
        }
        if bus.clk() {
            pb &= !0x04; // CLK released -> inverted PB2 = 0
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
    /// PB1 and PB3 are the DOS's DATA and CLK outputs (1 = pull the line low).
    /// PB4 is **ATNA**, and it feeds a gate on the drive's board that also pulls
    /// DATA low — wired-OR with PB1.
    ///
    /// The gate pulls DATA low whenever **ATNA disagrees with ATN**, which is
    /// what makes a drive answer attention before its CPU has run. The DOS's
    /// handler then takes the hold over in software and hands it back:
    ///
    /// ```text
    ///   E870:  JSR $E9A5     ; PB1 = 1  -- pull DATA low in *software*
    ///   E873:  ORA #$10      ; ATNA = 1 -- and hand the hold to the gate
    ///   ...
    ///   E9D7:  JSR $E99C     ; PB1 = 0  -- release the software hold
    ///   E9DC:  JMP $FF20     ; then WAIT for DATA to read low
    /// ```
    ///
    /// Read that with the inverted inputs of [`Board::sync_bus_in`] in mind and
    /// it fits: setting ATNA makes the gate agree with ATN and stop pulling, PB1
    /// keeps the line down meanwhile, and the `$FF20` wait is the DOS confirming
    /// that letting go of PB1 really did release the line before it starts
    /// clocking bits in.
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
            pulls |= iec::line::DATA;
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
    /// rotate the disk under the head, and service an IRQ if either VIA is
    /// asserting one. Returns those cycles.
    ///
    /// The disk's BYTE-READY line is wired to the CPU's SO pin on real hardware,
    /// setting the V flag the moment a byte arrives; we reproduce that by setting
    /// `cpu.v` when the mechanism reports a fresh byte, which is what makes the
    /// DOS's `BVC` read loop advance.
    pub fn step(&mut self) -> u8 {
        let cycles = self.cpu.step(&mut self.board);
        self.board.via1.tick(cycles as u32);
        self.board.via2.tick(cycles as u32);
        if self.board.disk.tick(&mut self.board.via2, cycles as u32) {
            self.cpu.v = true; // BYTE-READY -> SO pin -> V flag
        }
        if self.board.via1.irq_asserted() || self.board.via2.irq_asserted() {
            self.cpu.irq(&mut self.board);
        }
        cycles
    }

    /// Insert a disk into the drive.
    pub fn insert_disk(&mut self, disk: Disk) {
        self.board.disk.insert(disk);
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

    /// Run the drive until it reaches its idle loop (a healthy, booted DOS with
    /// its controller interrupt running). Panics if it never gets there.
    fn boot(m: &mut Machine) {
        for _ in 0..3_000_000u64 {
            m.step();
            if m.cpu.pc == IDLE_LOOP_HEAD {
                return;
            }
        }
        panic!("drive never reached the idle loop; stuck near ${:04X}", m.cpu.pc);
    }

    /// 1b milestone: the booted DOS reads a real sector off the emulated disk
    /// through its own job queue — the whole read path end to end. Posting a
    /// READ job makes the controller seek to the track (stepping the head and
    /// confirming its position by reading headers), hunt for the sector's sync
    /// mark and header, then shift in and GCR-decode the 256-byte data block.
    ///
    /// A completion code of `$01` means success; anything else is a specific DOS
    /// error (`$02` header not found, `$03` no sync, `$05` data checksum, `$0B`
    /// ID mismatch, `$0F` no disk) — so this test doubles as a precise diagnostic
    /// if the GCR format or timing is off.
    #[test]
    fn dos_reads_a_sector_through_its_job_queue() {
        let mut drive = Machine::new();
        boot(&mut drive);

        // Insert a synthetic disk and post a READ of track 18 sector 0 (the BAM)
        // into buffer 1, whose data lands at $0400. The job queue: code at $01,
        // header (track/sector) at $08/$09.
        let image = d64::Disk::new(d64::fixtures::synthetic_image()).unwrap();
        let id = image.id();
        drive.insert_disk(Disk::from_d64(&image));
        // A raw READ job reads whatever track the head is on — the DOS's file
        // layer seeks first, through a separate path. Put the head on track 18 as
        // that seek would, so this test isolates the read + GCR-decode path.
        drive.board.disk.seek(18);

        // A raw READ job checks the ID in the header it reads (which the drive
        // lands at $16/$17) against the drive's master ID at $12/$13 (per drive,
        // indexed by $3E), rejecting a mismatch with $0B — see $F3F6. At power-up
        // that master ID is zero, so present a drive already initialised to this
        // disk. The drive reads the two header ID bytes into $16/$17 in disk
        // order [id[0], id[1]], so the master must use that same order.
        drive.board.ram[0x12] = id[0];
        drive.board.ram[0x13] = id[1];

        drive.board.ram[0x08] = 18; // buffer 1 wants track 18
        drive.board.ram[0x09] = 0; //             sector 0
        drive.board.ram[0x01] = 0x80; // READ

        // Let the controller run the job to completion (it flips the code below
        // $80 when done).
        let mut code = 0x80u8;
        for _ in 0..8_000_000u64 {
            drive.step();
            code = drive.board.ram[0x01];
            if code < 0x80 {
                break;
            }
        }
        assert_eq!(code, 0x01, "read job returned error code ${code:02X}");

        // The BAM should now be in buffer 1: byte 0 = first directory track (18),
        // byte 2 = DOS version 'A'.
        assert_eq!(drive.board.ram[0x0400], 18, "BAM link track");
        assert_eq!(drive.board.ram[0x0402], b'A', "BAM DOS version byte");
    }

    /// 1a milestone: the drive boots on a shared serial bus and answers ATN. It
    /// releases DATA at idle, and pulls DATA low the moment the controller
    /// asserts attention — the hardware acknowledge plus the DOS taking its CA1
    /// interrupt.
    /// The DOS seeks the head to the track a job asks for.
    ///
    /// The dispatcher at `$F326` only seeks when it *knows* where the head is:
    /// it reads the current track from `$22`, and a zero there means "unknown",
    /// so it skips the seek entirely. At power-up `$22` is zero — which is why
    /// the read test above can post a raw job and have it read whatever track
    /// the head happens to be on. Tell the DOS where the head is and it steps.
    ///
    /// This is the test that pins the stepper *direction* down. Get it backwards
    /// and the head walks to the rim stop and sits there, so a seek from 1 to 18
    /// converging is the proof.
    #[test]
    fn dos_seeks_the_head_to_the_track_a_job_asks_for() {
        let mut drive = Machine::new();
        boot(&mut drive);

        let image = d64::Disk::new(d64::fixtures::synthetic_image()).unwrap();
        let id = image.id();
        drive.insert_disk(Disk::from_d64(&image));

        // Head parked on track 1, and the DOS believes it ($22).
        drive.board.disk.seek(1);
        drive.board.ram[0x22] = 1;
        // Master ID, as an initialised drive would have (see the read test).
        drive.board.ram[0x12] = id[0];
        drive.board.ram[0x13] = id[1];

        // Ask buffer 1 for track 18 sector 0 — seventeen tracks inward.
        drive.board.ram[0x08] = 18;
        drive.board.ram[0x09] = 0;
        drive.board.ram[0x01] = 0x80; // READ

        let mut code = 0x80u8;
        let mut min_track = 1u8;
        let mut max_track = 1u8;
        for _ in 0..20_000_000u64 {
            drive.step();
            let t = drive.board.disk.track();
            min_track = min_track.min(t);
            max_track = max_track.max(t);
            code = drive.board.ram[0x01];
            if code < 0x80 {
                break;
            }
        }

        assert_eq!(
            drive.board.disk.track(),
            18,
            "head ended on track {} (ranged {min_track}..={max_track}) — \
             if it stuck at 1 the stepper is going the wrong way",
            drive.board.disk.track()
        );
        assert_eq!(code, 0x01, "read job returned error code ${code:02X}");
        assert_eq!(drive.board.ram[0x0400], 18, "BAM link track");
        assert_eq!(drive.board.ram[0x0402], b'A', "BAM DOS version byte");
    }

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
