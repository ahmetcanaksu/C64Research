//! A Commodore 64 machine.
//!
//! Wires the verified 6510 CPU to 64 KB RAM, the three ROMs, two CIAs, and the
//! VIC-II, with the 6510/PLA memory banking that makes it all coexist. This is
//! the C64 counterpart of the `c1541` drive machine — same pattern (CPU borrows
//! a `Board` as its bus; `step()` advances the chips and services interrupts),
//! scaled up to the C64's hardware.
//!
//! Banking rule: **writes always land in RAM**; reads see BASIC
//! ROM ($A000-$BFFF), the character generator or I/O ($D000-$DFFF), or KERNAL
//! ROM ($E000-$FFFF) depending on the LORAM/HIRAM/CHAREN bits of the 6510 port
//! at $0001. That write-hits-RAM rule is what lets the KERNAL's RAMTAS find the
//! top of RAM under the ROMs.
//!
//! `no_std`: the machine takes the ROM images as byte slices, so a `std` host
//! (or a test) loads the files and this crate stays MCU-friendly.

#![no_std]

use cia6526::Cia;
use mos6502::{Bus, Cpu};
use sid::Sid;
use vic2::Vic;

/// Joystick direction/fire bits for [`C64::set_joystick`]. The C64 reads a
/// joystick **active-low**, so a mask is built by *clearing* the bits that are
/// pressed, starting from [`joystick::CENTER`].
///
/// ```
/// use c64::joystick;
/// // Up-and-left with fire held:
/// let mask = joystick::CENTER & !(joystick::UP | joystick::LEFT | joystick::FIRE);
/// ```
pub mod joystick {
    pub const UP: u8 = 0x01;
    pub const DOWN: u8 = 0x02;
    pub const LEFT: u8 = 0x04;
    pub const RIGHT: u8 = 0x08;
    pub const FIRE: u8 = 0x10;
    /// Nothing pressed.
    pub const CENTER: u8 = 0xFF;
}

/// A cartridge plugged into the expansion port.
///
/// The port is not just extra ROM — it carries two lines, `EXROM` and `GAME`,
/// that the PLA feeds into its banking decisions. Between them and the 6510's
/// own LORAM/HIRAM they select one of a handful of layouts. Two matter enough to
/// model, and they cover the great majority of real cartridges:
///
/// | Kind | `EXROM` | `GAME` | Visible |
/// |------|---------|--------|---------|
/// | [`CartKind::Lo8k`] | low | high | 8 KB at `$8000` (ROML) |
/// | [`CartKind::Hi16k`] | low | low | 8 KB at `$8000` **and** 8 KB at `$A000` (ROMH) |
///
/// The 16 KB layout is the interesting one: its second half sits at `$A000`,
/// exactly where BASIC ROM lives, and **replaces it**. That is how a cartridge
/// game takes the machine over completely — there is no BASIC left to return to.
///
/// A cartridge that wants to boot itself puts the `CBM80` signature at `$8004`;
/// the KERNAL's reset finds it and jumps through the vector at `$8000` before
/// BASIC is ever started (see `kernal::reset::cartridge_present`).
pub struct Cartridge {
    kind: CartKind,
    /// ROML: the 8 KB seen at $8000-$9FFF.
    roml: [u8; 0x2000],
    /// ROMH: the 8 KB seen at $A000-$BFFF, for a 16 KB cartridge.
    romh: [u8; 0x2000],
}

/// Which expansion-port configuration a cartridge asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CartKind {
    /// 8 KB at `$8000` only.
    Lo8k,
    /// 16 KB: `$8000` plus `$A000`, displacing BASIC ROM.
    Hi16k,
}

impl Cartridge {
    /// An 8 KB cartridge at `$8000`. Shorter images are zero-padded.
    pub fn lo8k(rom: &[u8]) -> Self {
        let mut c = Cartridge { kind: CartKind::Lo8k, roml: [0; 0x2000], romh: [0; 0x2000] };
        let n = rom.len().min(0x2000);
        c.roml[..n].copy_from_slice(&rom[..n]);
        c
    }

    /// A 16 KB cartridge: the first 8 KB at `$8000`, the second at `$A000`.
    pub fn hi16k(rom: &[u8]) -> Self {
        let mut c = Cartridge { kind: CartKind::Hi16k, roml: [0; 0x2000], romh: [0; 0x2000] };
        let lo = rom.len().min(0x2000);
        c.roml[..lo].copy_from_slice(&rom[..lo]);
        if rom.len() > 0x2000 {
            let hi = (rom.len() - 0x2000).min(0x2000);
            c.romh[..hi].copy_from_slice(&rom[0x2000..0x2000 + hi]);
        }
        c
    }

    /// Which configuration this cartridge asserts.
    pub fn kind(&self) -> CartKind {
        self.kind
    }

    /// Does this cartridge ask to be started at reset (`CBM80` at `$8004`)?
    pub fn is_autostart(&self) -> bool {
        self.roml[0x04..0x09] == [0xC3, 0xC2, 0xCD, 0x38, 0x30]
    }
}

/// Everything on the C64's bus.
pub struct Board {
    pub ram: [u8; 0x1_0000],
    pub kernal: [u8; 0x2000],
    pub basic: [u8; 0x2000],
    pub chargen: [u8; 0x1000],
    pub cia1: Cia,
    pub cia2: Cia,
    pub vic: Vic,
    pub sid: Sid,
    /// 6510 on-chip I/O port: data-direction ($0000) and data ($0001).
    port_ddr: u8,
    port_data: u8,
    /// Keyboard matrix: `key_matrix[c]` bit `r` set means the key wired to CIA1
    /// port-A select line `c` and port-B return line `r` is held down. Index by
    /// the KERNAL matrix code: line = idx/8, bit = idx%8.
    pub key_matrix: [u8; 8],
    /// Joystick states, **active-low**: a clear bit means pressed. Bits
    /// 0-4 = up/down/left/right/fire; default `$FF` (centered, not firing).
    /// Port 2 shares CIA1 port A ($DC00), port 1 shares port B ($DC01) — the same
    /// pins the keyboard uses, so a joystick pulls those read lines low.
    pub joy1: u8,
    pub joy2: u8,
    /// The serial (IEC) bus hanging off CIA2. The C64 occupies
    /// [`iec::CONTROLLER`]; a drive attaches to another slot and is clocked by
    /// whoever owns both machines (see [`C64::step`]).
    pub iec: iec::Bus,
    /// A cartridge in the expansion port, if one is plugged in.
    cart: Option<Cartridge>,
}

impl Board {
    /// Build a board from the three ROM images (KERNAL 8K, BASIC 8K, CHARGEN 4K).
    pub fn new(kernal: &[u8], basic: &[u8], chargen: &[u8]) -> Self {
        let mut b = Board {
            ram: [0; 0x1_0000],
            kernal: [0; 0x2000],
            basic: [0; 0x2000],
            chargen: [0; 0x1000],
            cia1: Cia::new(),
            cia2: Cia::new(),
            vic: Vic::new(),
            sid: Sid::new(),
            port_ddr: 0x00,
            port_data: 0x00,
            key_matrix: [0; 8],
            joy1: 0xFF,
            joy2: 0xFF,
            iec: iec::Bus::new(),
            cart: None,
        };
        b.kernal.copy_from_slice(&kernal[..0x2000]);
        b.basic.copy_from_slice(&basic[..0x2000]);
        b.chargen.copy_from_slice(&chargen[..0x1000]);
        b
    }

    /// Effective LORAM/HIRAM/CHAREN bits: an output bit reads its latch, an input
    /// bit floats high (pull-ups on bits 0-2), giving the default $37 at reset.
    fn bank_bits(&self) -> u8 {
        (self.port_data | !self.port_ddr) & 0x07
    }
    fn loram(&self) -> bool {
        self.bank_bits() & 0x01 != 0
    }
    fn hiram(&self) -> bool {
        self.bank_bits() & 0x02 != 0
    }
    fn charen(&self) -> bool {
        self.bank_bits() & 0x04 != 0
    }
    /// I/O is visible at $D000-$DFFF when CHAREN is set and any ROM is banked.
    fn io_mapped(&self) -> bool {
        self.charen() && (self.loram() || self.hiram())
    }

    /// The 6510 port at `$01` as the CPU reads it (banking latch).
    pub fn port01(&self) -> u8 {
        (self.port_data & self.port_ddr) | !self.port_ddr
    }

    /// What is currently banked in, for a status/monitor display:
    /// `(basic @ $A000, kernal @ $E000, io @ $D000, chargen @ $D000)`.
    pub fn banking(&self) -> (bool, bool, bool, bool) {
        let any_rom = self.loram() || self.hiram();
        (
            self.loram() && self.hiram(), // BASIC
            self.hiram(),                 // KERNAL
            self.io_mapped(),             // I/O
            !self.charen() && any_rom,    // CHARGEN
        )
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            0xD000..=0xD3FF => self.vic.read(addr as u8 & 0x3F),
            0xD400..=0xD7FF => self.sid.read(addr as u8 & 0x1F),
            0xD800..=0xDBFF => self.ram[addr as usize] & 0x0F | 0xF0, // colour RAM (4-bit)
            0xDC00..=0xDCFF => match addr as u8 & 0x0F {
                // Port A ($DC00): keyboard column drive, but a joystick in port 2
                // pulls its pins low regardless — so AND its mask into the read.
                0x00 => self.cia1.read(0) & self.joy2,
                // Port B ($DC01): the keyboard rows for whichever columns port A
                // is driving low, plus a joystick in port 1 on the same lines.
                0x01 => {
                    self.cia1.pb_in = self.keyboard_prb() & self.joy1;
                    self.cia1.read(1)
                }
                reg => self.cia1.read(reg),
            },
            0xDD00..=0xDDFF => {
                // Reading CIA2 port A samples the two serial-bus input lines.
                if addr as u8 & 0x0F == 0x00 {
                    self.sync_iec_in();
                }
                self.cia2.read(addr as u8 & 0x0F)
            }
            _ => 0, // I/O expansion
        }
    }

    /// Sample the serial bus into CIA2's port-A input pins.
    ///
    /// `$DD00` bit 6 is CLK IN and bit 7 is DATA IN, and unlike the *output*
    /// bits below they are **not** inverted: the bit reads `1` when the line is
    /// high (released) and `0` when some device is pulling it low. The C64 sees
    /// its own output pulls here too — they're the same three wires — which is
    /// exactly how the KERNAL's handshake loops expect to read the bus back.
    fn sync_iec_in(&mut self) {
        // Bits 0-5 are outputs, so their input level is irrelevant (`port_a`
        // masks them out by DDRA); leave them floating high.
        let mut pa = 0xFF;
        if !self.iec.clk() {
            pa &= !0x40; // CLK pulled low -> bit 6 reads 0
        }
        if !self.iec.data() {
            pa &= !0x80; // DATA pulled low -> bit 7 reads 0
        }
        self.cia2.pa_in = pa;
    }

    /// Push CIA2's serial-bus output bits onto the bus.
    ///
    /// `$DD00` bits 3/4/5 are ATN/CLK/DATA out, and the 7406 buffers on the
    /// board **invert** them: a `1` in the register pulls its line **low**. So
    /// the KERNAL's `ORA #$08` to "assert ATN" is really "pull ATN to ground",
    /// and a register of all zeroes is an idle bus with every line released.
    ///
    /// Called after anything that can change the port's output level — a write
    /// to the port register or to its data-direction register.
    fn sync_iec_out(&mut self) {
        let pra = self.cia2.port_a();
        let mut pulls = 0;
        if pra & 0x08 != 0 {
            pulls |= iec::line::ATN;
        }
        if pra & 0x10 != 0 {
            pulls |= iec::line::CLK;
        }
        if pra & 0x20 != 0 {
            pulls |= iec::line::DATA;
        }
        self.iec.set_pulls(iec::CONTROLLER, pulls);
    }

    /// Read a byte the way the CPU would, for tests and debuggers.
    ///
    /// Goes through the real banking, so it sees what the CPU sees rather than
    /// raw RAM. Note it is **not** side-effect free: reading an I/O register
    /// here clears the same latches it would clear for the CPU, because it is
    /// the same read. That is deliberate — a "peek" that quietly bypassed the
    /// hardware would lie about the machine.
    pub fn peek(&mut self, addr: u16) -> u8 {
        <Self as Bus>::read(self, addr)
    }

    /// Plug a cartridge into the expansion port (or unplug with `None`).
    pub fn set_cartridge(&mut self, cart: Option<Cartridge>) {
        self.cart = cart;
    }

    /// The cartridge currently plugged in, if any.
    pub fn cartridge(&self) -> Option<&Cartridge> {
        self.cart.as_ref()
    }

    /// Is a 16 KB cartridge asserting GAME, taking over `$A000`?
    fn cart_hi(&self) -> bool {
        matches!(self.cart.as_ref().map(|c| c.kind), Some(CartKind::Hi16k))
    }

    /// Compute the CIA1 port-B keyboard return lines from the matrix and the
    /// columns currently selected (driven low) on port A.
    fn keyboard_prb(&self) -> u8 {
        let pra = self.cia1.port_a();
        let mut prb = 0xFF;
        for col in 0..8 {
            if pra & (1 << col) == 0 {
                prb &= !self.key_matrix[col];
            }
        }
        prb
    }

    /// Press (`true`) or release a key by its KERNAL matrix code (0..63).
    pub fn set_key(&mut self, matrix_code: u8, pressed: bool) {
        if matrix_code >= 64 {
            return;
        }
        let (line, bit) = ((matrix_code / 8) as usize, matrix_code % 8);
        if pressed {
            self.key_matrix[line] |= 1 << bit;
        } else {
            self.key_matrix[line] &= !(1 << bit);
        }
    }

    fn io_write(&mut self, addr: u16, val: u8) {
        match addr {
            0xD000..=0xD3FF => self.vic.write(addr as u8 & 0x3F, val),
            0xD400..=0xD7FF => self.sid.write(addr as u8 & 0x1F, val),
            0xD800..=0xDBFF => self.ram[addr as usize] = val & 0x0F, // colour RAM
            0xDC00..=0xDCFF => self.cia1.write(addr as u8 & 0x0F, val),
            0xDD00..=0xDDFF => {
                let reg = addr as u8 & 0x0F;
                self.cia2.write(reg, val);
                // Port A or its direction register changed the levels the C64
                // is driving onto the serial bus.
                if reg == 0x00 || reg == 0x02 {
                    self.sync_iec_out();
                }
            }
            _ => {}
        }
    }

    /// CIA2 port A — its low two bits pick the VIC's 16 KB bank (used by the
    /// renderer).
    pub fn cia2_pra(&self) -> u8 {
        self.cia2.port_a()
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000 => self.port_ddr,
            0x0001 => (self.port_data & self.port_ddr) | (!self.port_ddr),
            // ROML: an 8 KB cartridge appears here whenever the 6510 has
            // HIRAM set — which it does in the machine's normal state, so a
            // cartridge is visible from reset onwards.
            0x8000..=0x9FFF => match self.cart.as_ref() {
                Some(c) if self.hiram() => c.roml[(addr - 0x8000) as usize],
                _ => self.ram[addr as usize],
            },
            0xA000..=0xBFFF => {
                // ROMH *replaces* BASIC: a 16 KB cartridge wins this region, and
                // that is precisely how it stops BASIC from ever running.
                if self.cart_hi() && self.hiram() {
                    let c = self.cart.as_ref().expect("cart_hi implies a cartridge");
                    c.romh[(addr - 0xA000) as usize]
                } else if self.loram() && self.hiram() {
                    self.basic[(addr - 0xA000) as usize]
                } else {
                    self.ram[addr as usize]
                }
            }
            0xD000..=0xDFFF => {
                if self.loram() || self.hiram() {
                    if self.charen() {
                        self.io_read(addr)
                    } else {
                        self.chargen[(addr - 0xD000) as usize]
                    }
                } else {
                    self.ram[addr as usize]
                }
            }
            0xE000..=0xFFFF => {
                if self.hiram() {
                    self.kernal[(addr - 0xE000) as usize]
                } else {
                    self.ram[addr as usize]
                }
            }
            _ => self.ram[addr as usize],
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000 => self.port_ddr = val,
            0x0001 => self.port_data = val,
            0xD000..=0xDFFF if self.io_mapped() => self.io_write(addr, val),
            // Everything else — including under the ROMs — writes to RAM.
            _ => self.ram[addr as usize] = val,
        }
    }
}

/// The whole machine: CPU + board.
pub struct C64 {
    pub cpu: Cpu,
    pub board: Board,
    prev_nmi: bool,
}

impl C64 {
    /// Build and reset a fresh C64 from its ROM images.
    pub fn new(kernal: &[u8], basic: &[u8], chargen: &[u8]) -> Self {
        let mut m = C64 {
            cpu: Cpu::new(),
            board: Board::new(kernal, basic, chargen),
            prev_nmi: false,
        };
        m.cpu.reset(&mut m.board);
        m
    }

    /// Execute one instruction, advance the chips by the cycles it took, and
    /// service interrupts (CIA1 -> IRQ, CIA2 -> NMI on its rising edge).
    pub fn step(&mut self) -> u8 {
        let cycles = self.cpu.step(&mut self.board);
        let c = cycles as u32;
        self.board.cia1.tick(c);
        self.board.cia2.tick(c);
        self.board.vic.tick(c);
        self.board.sid.clock(c);

        if self.board.cia1.irq_asserted() || self.board.vic.irq_asserted() {
            self.cpu.irq(&mut self.board);
        }
        let nmi = self.board.cia2.irq_asserted();
        if nmi && !self.prev_nmi {
            self.cpu.nmi(&mut self.board);
        }
        self.prev_nmi = nmi;

        cycles
    }

    /// Load a `.prg` image (first two bytes = little-endian load address, rest =
    /// data) straight into RAM. If it's a BASIC program (load address $0801),
    /// also fix the BASIC end-of-program pointers so `RUN`/`LIST` see it.
    /// Returns the load address. Do this once the machine has booted to READY.
    pub fn load_prg(&mut self, prg: &[u8]) -> u16 {
        if prg.len() < 2 {
            return 0;
        }
        let load = u16::from_le_bytes([prg[0], prg[1]]);
        let data = &prg[2..];
        for (i, &b) in data.iter().enumerate() {
            self.board.ram[(load as usize + i) & 0xFFFF] = b;
        }
        let end = load.wrapping_add(data.len() as u16);
        if load == 0x0801 {
            let [lo, hi] = end.to_le_bytes();
            for p in [0x2D, 0x2F, 0x31] {
                // VARTAB / ARYTAB / STREND all point just past the program.
                self.board.ram[p] = lo;
                self.board.ram[p + 1] = hi;
            }
        }
        load
    }

    /// Plug a cartridge in and reset, so the KERNAL's reset finds it.
    ///
    /// The order matters: a cartridge is only *started* by the check at `$FD02`
    /// during reset, so plugging one into a running machine changes what memory
    /// looks like but does not hand control over. Resetting afterwards is what
    /// makes an autostart cartridge actually boot.
    pub fn insert_cartridge(&mut self, cart: Cartridge) {
        self.board.set_cartridge(Some(cart));
        self.cpu.reset(&mut self.board);
    }

    /// Trigger an NMI — the C64's RESTORE key is wired to the CPU's NMI line, so
    /// this is how a host binds it. (RUN/STOP held + RESTORE = warm reset, which
    /// the KERNAL's NMI handler does on its own.)
    pub fn nmi(&mut self) {
        self.cpu.nmi(&mut self.board);
    }

    /// Set a joystick's state. `port` is 1 or 2; `mask` is active-low with bits
    /// 0-4 = up/down/left/right/fire (a clear bit = pressed), so `$FF` is
    /// centered and not firing. Build it from the [`joystick`] constants.
    pub fn set_joystick(&mut self, port: u8, mask: u8) {
        match port {
            1 => self.board.joy1 = mask,
            2 => self.board.joy2 = mask,
            _ => {}
        }
    }

    /// Render the whole display in one shot with the current VIC state
    /// (no mid-frame raster effects). Handy for tests and simple use.
    ///
    /// `&mut self` because rendering is when the VIC detects sprite collisions.
    pub fn render(&mut self, fb: &mut [u32]) {
        let cia2 = self.board.cia2_pra();
        for y in 0..vic2::HEIGHT {
            self.board
                .vic
                .render_line(y, &self.board.ram, &self.board.chargen, cia2, fb);
        }
    }

    /// Run one video frame, rendering each scanline as the raster reaches it so
    /// a raster-interrupt handler that reprograms the VIC mid-frame produces the
    /// correct split. This is the path a live emulator should use.
    pub fn run_frame(&mut self, fb: &mut [u32]) {
        let deadline = self.cpu.cycles.wrapping_add(19_700); // ~one PAL frame
        let mut rendered = [false; vic2::HEIGHT];
        while self.cpu.cycles < deadline {
            let line = self.board.vic.raster() as usize;
            if line < vic2::HEIGHT && !rendered[line] {
                let cia2 = self.board.cia2_pra();
                self.board
                    .vic
                    .render_line(line, &self.board.ram, &self.board.chargen, cia2, fb);
                rendered[line] = true;
            }
            self.step();
        }
        // Fill any lines the raster didn't visit this pass with the final state.
        let cia2 = self.board.cia2_pra();
        for (y, drawn) in rendered.iter().enumerate() {
            if !drawn {
                self.board
                    .vic
                    .render_line(y, &self.board.ram, &self.board.chargen, cia2, fb);
            }
        }
    }
}

#[cfg(test)]
mod cartridge_tests {
    use super::*;

    /// Blank ROMs: these tests are about *banking*, so what the ROMs contain
    /// doesn't matter — only which of them the CPU can see.
    fn board() -> Board {
        Board::new(&[0x11; 0x2000], &[0x22; 0x2000], &[0; 0x1000])
    }

    #[test]
    fn without_a_cartridge_8000_is_plain_ram() {
        let mut b = board();
        b.ram[0x8000] = 0x5A;
        assert_eq!(b.read(0x8000), 0x5A);
        assert_eq!(b.read(0xA000), 0x22, "BASIC ROM is visible at $A000");
    }

    #[test]
    fn an_8k_cartridge_appears_at_8000_and_leaves_basic_alone() {
        let mut b = board();
        let mut rom = [0u8; 0x2000];
        rom[0] = 0xAB;
        b.set_cartridge(Some(Cartridge::lo8k(&rom)));

        assert_eq!(b.read(0x8000), 0xAB, "ROML is visible");
        assert_eq!(b.read(0xA000), 0x22, "an 8K cartridge must not displace BASIC");
    }

    /// The defining property of a 16 KB cartridge: it takes `$A000` away from
    /// BASIC, which is how it owns the machine.
    #[test]
    fn a_16k_cartridge_replaces_basic_rom() {
        let mut b = board();
        let mut rom = [0u8; 0x4000];
        rom[0] = 0xAB; // ROML
        rom[0x2000] = 0xCD; // ROMH
        b.set_cartridge(Some(Cartridge::hi16k(&rom)));

        assert_eq!(b.read(0x8000), 0xAB, "ROML at $8000");
        assert_eq!(b.read(0xA000), 0xCD, "ROMH replaces BASIC at $A000");
    }

    /// Writes still go to the RAM underneath, exactly as they do beneath the
    /// ROMs — the rule RAMTAS depends on.
    #[test]
    fn writes_under_a_cartridge_reach_ram() {
        let mut b = board();
        let mut rom = [0u8; 0x2000];
        rom[0] = 0xAB;
        b.set_cartridge(Some(Cartridge::lo8k(&rom)));

        b.write(0x8000, 0x77);
        assert_eq!(b.read(0x8000), 0xAB, "the read still sees the cartridge");
        // Bank the cartridge out (HIRAM low) and the write is revealed.
        b.write(0x0000, 0xFF); // all outputs
        b.write(0x0001, 0x00); // LORAM/HIRAM/CHAREN all low
        assert_eq!(b.read(0x8000), 0x77, "the write landed in RAM underneath");
    }

    #[test]
    fn cbm80_signature_marks_an_autostart_cartridge() {
        let mut rom = [0u8; 0x2000];
        assert!(!Cartridge::lo8k(&rom).is_autostart());
        rom[0x04..0x09].copy_from_slice(&[0xC3, 0xC2, 0xCD, 0x38, 0x30]);
        assert!(Cartridge::lo8k(&rom).is_autostart());
    }

    #[test]
    fn a_short_image_is_zero_padded() {
        let c = Cartridge::lo8k(&[0x01, 0x02]);
        assert_eq!(c.roml[0], 0x01);
        assert_eq!(c.roml[2], 0x00, "the rest reads as zero rather than panicking");
    }
}

#[cfg(test)]
mod joystick_tests {
    use super::*;

    /// Blank ROMs — these tests only exercise the CIA1 port reads.
    fn board() -> Board {
        Board::new(&[0; 0x2000], &[0; 0x2000], &[0; 0x1000])
    }

    #[test]
    fn joystick2_pulls_dc00_bits_low() {
        let mut b = board();
        // Up + fire held (active-low: clear those bits).
        b.joy2 = joystick::CENTER & !(joystick::UP | joystick::FIRE);
        let v = b.read(0xDC00); // I/O is mapped at reset ($37 banking)
        assert_eq!(v & joystick::UP, 0, "up is pressed -> bit low");
        assert_eq!(v & joystick::FIRE, 0, "fire is pressed -> bit low");
        assert_eq!(v & joystick::DOWN, joystick::DOWN, "down is released -> bit high");
        assert_eq!(v & joystick::RIGHT, joystick::RIGHT, "right is released -> bit high");
    }

    #[test]
    fn joystick1_and_the_keyboard_share_dc01() {
        let mut b = board();
        b.joy1 = joystick::CENTER & !joystick::LEFT; // left held on joystick 1
        let v = b.read(0xDC01);
        assert_eq!(v & joystick::LEFT, 0, "joystick 1 pulls its DATA line low on $DC01");
    }

    #[test]
    fn centered_joystick_reads_all_ones() {
        let mut b = board();
        // No keys, no joystick: $DC00 low bits all high (nothing pressed).
        assert_eq!(b.read(0xDC00) & 0x1F, 0x1F);
    }
}
