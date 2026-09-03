//! C64 KERNAL routines, reimplemented as **readable Rust — reference material**.
//!
//! This crate is *not* used by the emulator. It exists to make the KERNAL
//! legible: each routine here is a faithful, line-by-line translation of the
//! original 6502 code (disassembled from `kernal-901227-03.bin` with the repo's
//! `disasm` tool), with the source listing quoted right above the Rust so you
//! can check the translation against the ROM.
//!
//! It's the "understand" half of the disassemble → understand → port pipeline,
//! turned into study notes you can actually run and test.
//!
//! See `docs/kernal-boot.md` for the narrated walk-through of the whole boot.
//!
//! Routines currently translated:
//! - [`reset`] — RESET (`$FCE2`), **the KERNAL main**: the whole power-on boot
//!   chain (cartridge check, IOINIT, RAMTAS, RESTOR, CINT) ending at the
//!   `JMP ($A000)` hand-off to BASIC.
//! - [`irq`] — the default IRQ handler (`$EA31`) and keyboard scan (`$EA87`):
//!   what runs 60 times a second once interrupts are enabled.
//! - [`basic`] — the BASIC cold-start seam (`$E394`): the KERNAL-resident glue
//!   that sets up BASIC and prints the startup banner before the interpreter
//!   takes over. Stops at the KERNAL/BASIC-ROM boundary.
//! - [`vectors`] — RESTOR / VECTOR (`$FD15`/`$FD1A`): install & fetch the RAM
//!   I/O vectors. This is *why* so much of the KERNAL is reached indirectly.
//! - [`ramtas`] — RAMTAS (`$FD50`): power-on RAM test, clear, and size.
//! - [`udtim`] — UDTIM (`$F69B`): advance the jiffy clock and scan the STOP key.

pub mod basic;
pub mod irq;
pub mod ramtas;
pub mod reset;
pub mod udtim;
pub mod vectors;

/// A 64 KB address space that models the one C64 banking rule the boot code
/// depends on: **writes always land in RAM, but reads in a banked ROM region
/// return the ROM.** That asymmetry is exactly how RAMTAS discovers the top of
/// RAM — it writes a test pattern, reads it back, and where a ROM shadows the
/// read the pattern "doesn't stick".
///
/// When `rom` is set it shadows reads in the BASIC ($A000-$BFFF) and KERNAL
/// ($E000-$FFFF) regions; the I/O area ($D000-$DFFF) and everything else always
/// read/write plain RAM. `rom` is `None` by default (all RAM).
pub struct C64Mem {
    pub ram: [u8; 0x1_0000],
    /// Optional 64 KB ROM image; only its $A000-$BFFF and $E000-$FFFF bytes are
    /// ever consulted (on reads).
    pub rom: Option<Vec<u8>>,
}

/// True for the address ranges where banked-in ROM shadows reads at boot.
#[inline]
fn is_rom_region(addr: u16) -> bool {
    matches!(addr, 0xA000..=0xBFFF | 0xE000..=0xFFFF)
}

impl Default for C64Mem {
    fn default() -> Self {
        C64Mem { ram: [0; 0x1_0000], rom: None }
    }
}

impl C64Mem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a byte (`LDA`, `CMP`, …). Returns ROM in the banked ROM regions.
    #[inline]
    pub fn r(&self, addr: u16) -> u8 {
        if let Some(rom) = &self.rom {
            if is_rom_region(addr) {
                return rom[addr as usize];
            }
        }
        self.ram[addr as usize]
    }

    /// Write a byte (`STA`, …). Always writes RAM — even "under" a ROM.
    #[inline]
    pub fn w(&mut self, addr: u16, val: u8) {
        self.ram[addr as usize] = val;
    }

    /// The 16-bit little-endian pointer stored at zero page `zp`/`zp+1` — the
    /// base used by `($zp),Y` indirect-indexed addressing.
    #[inline]
    pub fn zp_ptr(&self, zp: u8) -> u16 {
        u16::from_le_bytes([self.r(zp as u16), self.r(zp.wrapping_add(1) as u16)])
    }

    /// The 16-bit little-endian word stored at any address `addr`/`addr+1`.
    #[inline]
    pub fn zp_ptr_at(&self, addr: u16) -> u16 {
        u16::from_le_bytes([self.r(addr), self.r(addr.wrapping_add(1))])
    }
}

// ---- Named zero-page / KERNAL locations referenced by the routines below ----

/// TIME, the 24-bit jiffy clock: `$A0` high, `$A1` middle, `$A2` low.
pub const TIME_HI: u16 = 0x00A0;
pub const TIME_MID: u16 = 0x00A1;
pub const TIME_LO: u16 = 0x00A2;
/// STKEY — the latched STOP-key column read by UDTIM.
pub const STKEY: u16 = 0x0091;

/// CINV — base of the 16-entry RAM vector table ($0314..$0333).
pub const CINV: u16 = 0x0314;

/// CIA #1 ports (keyboard matrix).
pub const CIA1_PRA: u16 = 0xDC00;
pub const CIA1_PRB: u16 = 0xDC01;

/// MEMSTR/MEMSIZ — OS start-of-memory ($0281/$0282) and top-of-memory
/// ($0283/$0284) pointers that RAMTAS fills in.
pub const MEMSTR_HI: u16 = 0x0282;
pub const MEMSIZ_LO: u16 = 0x0283;
pub const MEMSIZ_HI: u16 = 0x0284;
/// High byte of the default screen page ($0288 → $04 → screen at $0400).
pub const SCREEN_PAGE: u16 = 0x0288;

/// Test helper: a machine whose reads see banked-in ROM at $A000/$E000 (so
/// RAMTAS finds RAM ending at $A000). The ROM is synthetic filler except the
/// BASIC cold-start vector at $A000 → $E394. `#[doc(hidden)]`, tests only.
#[doc(hidden)]
pub fn test_machine_with_rom() -> C64Mem {
    let mut m = C64Mem::new();
    let mut rom = vec![0x60u8; 0x1_0000]; // filler that isn't the RAM-test pattern
    rom[0xA000] = 0x94; // BASIC cold-start vector lo
    rom[0xA001] = 0xE3; // BASIC cold-start vector hi -> $E394
    m.rom = Some(rom);
    m
}
