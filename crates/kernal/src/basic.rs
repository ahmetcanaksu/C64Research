//! The BASIC cold-start seam ($E394) — the glue between KERNAL and BASIC.
//!
//! After RESET does `JMP ($A000)`, control lands at `$E394`. Despite being
//! "BASIC cold start", these routines live in the **KERNAL ROM**: they set up
//! BASIC's zero page and RAM vectors, print the startup banner and free-memory
//! count, then hand off into the BASIC interpreter's main loop.
//!
//! This module ports that seam and **stops at the boundary**: the calls into the
//! BASIC ROM proper ($A000-$BFFF) — the string printer, the interpreter's main
//! loop — are noted but not reimplemented. The demonstrable result is the exact
//! startup text a real C64 shows, `38911 BASIC BYTES FREE` and all.
//!
//! ```text
//! E394:  20 53 E4  JSR  $E453    ; init_basic_vectors  ($0300 table)
//! E397:  20 BF E3  JSR  $E3BF    ; init_basic_ram      (zero page, pointers)
//! E39A:  20 22 E4  JSR  $E422    ; print banner + free bytes  (into BASIC ROM)
//! E39D:  A2 FB     LDX  #$FB
//! E39F:  9A        TXS           ; stack pointer for BASIC
//! E3A0:  D0 E4     BNE  $E386    ; -> LDX #$80; JMP ($0300)  (the READY/main entry)
//! ```

use crate::C64Mem;

/// BASIC's 6 indirect vectors, copied to $0300-$030B by `init_basic_vectors`.
/// $0300 IERROR=$E38B, $0302 IMAIN=$A483, $0304 ICRNCH=$A57C, $0306 IQPLOP=$A71A,
/// $0308 IGONE=$A7E4, $030A IEVAL=$AE86.
pub const BASIC_VECTORS: [u8; 12] =
    [0x8B, 0xE3, 0x83, 0xA4, 0x7C, 0xA5, 0x1A, 0xA7, 0xE4, 0xA7, 0x86, 0xAE];

/// The CHRGET routine (BASIC's "fetch next program byte"), copied into zero page
/// at $73 by `init_basic_ram`. It lives in RAM so BASIC can self-modify the
/// program pointer it holds.
pub const CHRGET_ROUTINE: [u8; 29] = [
    0xE6, 0x7A, 0xD0, 0x02, 0xE6, 0x7B, 0xAD, 0x60, 0xEA, 0xC9, 0x3A, 0xB0, 0x0A, 0xC9, 0x20, 0xF0,
    0xEF, 0x38, 0xE9, 0x30, 0x38, 0xE9, 0xD0, 0x60, 0x80, 0x4F, 0xC7, 0x52, 0x58,
];

/// The startup banner string ($E473, PETSCII): clear-screen, then
/// "    **** COMMODORE 64 BASIC V2 ****" and " 64K RAM SYSTEM  ".
const STARTUP_BANNER: &[u8] = &[
    0x93, 0x0D, 0x20, 0x20, 0x20, 0x20, 0x2A, 0x2A, 0x2A, 0x2A, 0x20, 0x43, 0x4F, 0x4D, 0x4D, 0x4F,
    0x44, 0x4F, 0x52, 0x45, 0x20, 0x36, 0x34, 0x20, 0x42, 0x41, 0x53, 0x49, 0x43, 0x20, 0x56, 0x32,
    0x20, 0x2A, 0x2A, 0x2A, 0x2A, 0x0D, 0x0D, 0x20, 0x36, 0x34, 0x4B, 0x20, 0x52, 0x41, 0x4D, 0x20,
    0x53, 0x59, 0x53, 0x54, 0x45, 0x4D, 0x20, 0x20,
];

/// The " BASIC BYTES FREE\r" string ($E460, PETSCII).
const FREE_MSG: &[u8] = &[
    0x20, 0x42, 0x41, 0x53, 0x49, 0x43, 0x20, 0x42, 0x59, 0x54, 0x45, 0x53, 0x20, 0x46, 0x52, 0x45,
    0x45, 0x0D,
];

/// What the cold-start seam produced.
pub struct ColdStart {
    /// The startup text as it would appear on screen (PETSCII decoded).
    pub banner: String,
    /// Address the seam jumps through next (`JMP ($0300)` = IERROR $E38B), which
    /// prints `READY.` and enters BASIC's main loop at IMAIN ($A483). Everything
    /// from here on is the BASIC interpreter — out of scope for this crate.
    pub handoff: u16,
}

/// BASIC cold start ($E394): the whole seam.
pub fn cold_start(m: &mut C64Mem) -> ColdStart {
    init_basic_vectors(m); // JSR $E453
    init_basic_ram(m); // JSR $E3BF
    let banner = startup_banner(m); // JSR $E422 (+ its BASIC-ROM print calls)
                                    // LDX #$FB; TXS; then $E386: LDX #$80; JMP ($0300).
    let handoff = m.zp_ptr_at(0x0300);
    ColdStart { banner, handoff }
}

/// Install BASIC's indirect vectors into $0300-$030B ($E453).
///
/// ```text
/// E453: LDX #$0B
/// E455: LDA $E447,X; STA $0300,X; DEX; BPL $E455
/// E45E: RTS
/// ```
pub fn init_basic_vectors(m: &mut C64Mem) {
    for (i, &b) in BASIC_VECTORS.iter().enumerate() {
        m.w(0x0300 + i as u16, b);
    }
}

/// Initialize BASIC's zero page and RAM pointers ($E3BF): the USR() jump, the
/// float/int conversion vectors, the CHRGET routine, and TXTTAB / MEMSIZ /
/// FRETOP from the KERNAL's MEMBOT / MEMTOP.
pub fn init_basic_ram(m: &mut C64Mem) {
    // USR() call: a JMP opcode at $54 and $0310, vector -> $B248.
    m.w(0x0054, 0x4C);
    m.w(0x0310, 0x4C);
    m.w(0x0311, 0x48);
    m.w(0x0312, 0xB2);
    // ADRAY1/ADRAY2 float<->int conversion vectors.
    m.w(0x0005, 0x91);
    m.w(0x0006, 0xB3); // $B391
    m.w(0x0003, 0xAA);
    m.w(0x0004, 0xB1); // $B1AA
    // Copy CHRGET into zero page at $73.
    for (i, &b) in CHRGET_ROUTINE.iter().enumerate() {
        m.w(0x0073 + i as u16, b);
    }
    m.w(0x0053, 0x03);
    m.w(0x0068, 0x00);
    m.w(0x0013, 0x00);
    m.w(0x0018, 0x00);
    m.w(0x01FD, 0x01);
    m.w(0x01FC, 0x01);
    m.w(0x0016, 0x19);

    // TXTTAB = MEMBOT ($0281/$0282); MEMSIZ = FRETOP = MEMTOP ($0283/$0284).
    let (bx, by) = (m.r(0x0281), m.r(0x0282)); // MEMBOT ($FF9C, carry set = read)
    m.w(0x002B, bx);
    m.w(0x002C, by);
    let (tx, ty) = (m.r(0x0283), m.r(0x0284)); // MEMTOP ($FF99, carry set = read)
    m.w(0x0037, tx);
    m.w(0x0038, ty);
    m.w(0x0033, tx);
    m.w(0x0034, ty);
    // Write a 0 at the start of BASIC text, then bump TXTTAB past it ($0800->$0801).
    let txttab = m.zp_ptr_at(0x002B);
    m.w(txttab, 0x00);
    let bumped = txttab.wrapping_add(1);
    m.w(0x002B, bumped as u8);
    m.w(0x002C, (bumped >> 8) as u8);
}

/// Print the startup banner and free-byte count ($E422). The ROM does this via
/// BASIC-ROM calls (memory check $A408, print-string $AB1E, print-number $BDCD,
/// then $A644); here we compute the same text directly.
///
/// ```text
/// E422: LDA $2B; LDY $2C; JSR $A408        ; set top of BASIC / check memory
/// E429: LDA #$73; LDY #$E4; JSR $AB1E      ; print banner ($E473)
/// E430: LDA $37; SEC; SBC $2B; TAX
/// E436: LDA $38; SBC $2C; JSR $BDCD        ; print (MEMSIZ - TXTTAB) as decimal
/// E43D: LDA #$60; LDY #$E4; JSR $AB1E      ; print " BASIC BYTES FREE" ($E460)
/// E444: JMP $A644                          ; -> BASIC NEW, then RTS back
/// ```
pub fn startup_banner(m: &C64Mem) -> String {
    let mut out = String::new();
    decode_petscii(&mut out, STARTUP_BANNER);
    // Free bytes = MEMSIZ ($37/$38) - TXTTAB ($2B/$2C).
    let memsiz = m.zp_ptr_at(0x0037);
    let txttab = m.zp_ptr_at(0x002B);
    let free = memsiz.wrapping_sub(txttab);
    out.push_str(&free.to_string());
    decode_petscii(&mut out, FREE_MSG);
    out
}

/// Turn a PETSCII string into readable text: printable codes pass through,
/// carriage return ($0D) becomes a newline, and clear-screen ($93) is dropped.
fn decode_petscii(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        match b {
            0x0D => out.push('\n'),
            0x20..=0x5F => out.push(b as char), // ASCII-compatible PETSCII range
            _ => {} // control codes ($93 clear-screen, etc.)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reset::reset;
    use crate::test_machine_with_rom;

    #[test]
    fn cold_start_prints_the_banner_and_38911_free() {
        let mut m = test_machine_with_rom();
        // Full power-on first (RESET sets MEMBOT/MEMSIZ that the seam reads).
        assert_eq!(reset(&mut m), 0xE394); // hands off to BASIC cold start

        let cs = cold_start(&mut m);
        assert!(cs.banner.contains("**** COMMODORE 64 BASIC V2 ****"));
        assert!(cs.banner.contains("64K RAM SYSTEM"));
        // The famous number: $A000 (MEMSIZ) - $0801 (TXTTAB) = 38911.
        assert!(
            cs.banner.contains("38911 BASIC BYTES FREE"),
            "banner was:\n{}",
            cs.banner
        );
        // Next stop is IERROR ($E38B), which prints READY. and enters BASIC.
        assert_eq!(cs.handoff, 0xE38B);
    }

    #[test]
    fn basic_vectors_and_ram_are_initialized() {
        let mut m = test_machine_with_rom();
        reset(&mut m);
        init_basic_vectors(&mut m);
        init_basic_ram(&mut m);

        // IMAIN ($0302) points at BASIC's main loop $A483.
        assert_eq!(m.zp_ptr_at(0x0302), 0xA483);
        // USR jump opcode staged.
        assert_eq!(m.r(0x0054), 0x4C);
        // CHRGET copied into zero page.
        assert_eq!(m.r(0x0073), CHRGET_ROUTINE[0]);
        // TXTTAB = $0801, MEMSIZ = $A000, and $0800 is the leading zero byte.
        assert_eq!(m.zp_ptr_at(0x002B), 0x0801);
        assert_eq!(m.zp_ptr_at(0x0037), 0xA000);
        assert_eq!(m.r(0x0800), 0x00);
    }
}
