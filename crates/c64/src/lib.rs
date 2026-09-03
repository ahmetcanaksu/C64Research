//! A Commodore 64 machine.
//!
//! Wires the verified 6510 CPU to 64 KB RAM, the three ROMs, two CIAs, and the
//! VIC-II, with the 6510/PLA memory banking that makes it all coexist. This is
//! the C64 counterpart of the `c1541` drive machine — same pattern (CPU borrows
//! a `Board` as its bus; `step()` advances the chips and services interrupts),
//! scaled up to the C64's hardware.
//!
//! Banking rule (no cartridge): **writes always land in RAM**; reads see BASIC
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

    fn io_read(&mut self, addr: u16) -> u8 {
        match addr {
            0xD000..=0xD3FF => self.vic.read(addr as u8 & 0x3F),
            0xD400..=0xD7FF => self.sid.read(addr as u8 & 0x1F),
            0xD800..=0xDBFF => self.ram[addr as usize] & 0x0F | 0xF0, // colour RAM (4-bit)
            0xDC00..=0xDCFF => {
                // Reading CIA1 port B returns the keyboard rows for whichever
                // columns port A is currently driving low.
                if addr as u8 & 0x0F == 0x01 {
                    let prb = self.keyboard_prb();
                    self.cia1.pb_in = prb;
                }
                self.cia1.read(addr as u8 & 0x0F)
            }
            0xDD00..=0xDDFF => self.cia2.read(addr as u8 & 0x0F),
            _ => 0, // I/O expansion
        }
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
            0xDD00..=0xDDFF => self.cia2.write(addr as u8 & 0x0F, val),
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
            0xA000..=0xBFFF => {
                if self.loram() && self.hiram() {
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

    /// Trigger an NMI — the C64's RESTORE key is wired to the CPU's NMI line, so
    /// this is how a host binds it. (RUN/STOP held + RESTORE = warm reset, which
    /// the KERNAL's NMI handler does on its own.)
    pub fn nmi(&mut self) {
        self.cpu.nmi(&mut self.board);
    }

    /// Render the whole display in one shot with the current VIC state
    /// (no mid-frame raster effects). Handy for tests and simple use.
    pub fn render(&self, fb: &mut [u32]) {
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
        for y in 0..vic2::HEIGHT {
            if !rendered[y] {
                self.board
                    .vic
                    .render_line(y, &self.board.ram, &self.board.chargen, cia2, fb);
            }
        }
    }
}
