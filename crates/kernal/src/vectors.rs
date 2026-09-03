//! RESTOR and VECTOR — managing the RAM I/O vector table at $0314.
//!
//! The C64 routes its interrupt handlers and I/O calls (IRQ, BRK, NMI, OPEN,
//! CHKIN, CHROUT, …) through a table of 16 pointers in RAM at $0314-$0333.
//! On boot the KERNAL copies a table of *defaults* into it; a program can read
//! the current vectors back out or install its own — which is exactly how you
//! hook the IRQ or CHROUT. That indirection is why a plain trace of the ROM only
//! reaches a fraction of it: most routines are entered through these RAM slots.
//!
//! Original ROM listing:
//!
//! ```text
//! ; RESTOR ($FD15): point at the default table ($FD30) and fall into VECTOR
//! ; in "install" mode (carry clear).
//! FD15:  A2 30     LDX  #$30
//! FD17:  A0 FD     LDY  #$FD        ; X/Y = $FD30 (the default vector table)
//! FD19:  18        CLC              ; carry clear = copy list -> system
//! ; VECTOR ($FD1A): copy 32 bytes between the caller's list ($C3) and $0314.
//! FD1A:  86 C3     STX  $C3
//! FD1C:  84 C4     STY  $C4         ; ($C3) = caller's list
//! FD1E:  A0 1F     LDY  #$1F        ; 32 bytes, $1F..$00
//! FD20:  B9 14 03  LDA  $0314,Y     ; A = current system vector byte
//! FD23:  B0 02     BCS  $FD27       ; carry set (read): keep A = system byte
//! FD25:  B1 C3     LDA  ($C3),Y     ; carry clear (write): A = caller's byte
//! FD27:  91 C3     STA  ($C3),Y     ; store to the caller's list ...
//! FD29:  99 14 03  STA  $0314,Y     ; ... and to the system table
//! FD2C:  88        DEY
//! FD2D:  10 F1     BPL  $FD20
//! FD2F:  60        RTS
//! ```
//!
//! The single store path serves both directions: on a read the system byte is
//! written to both places (the system rewrite is a harmless no-op); on a write
//! the caller's byte is written to both (the list rewrite is the no-op).

use crate::{C64Mem, CINV};

/// VECTOR ($FD1A). Copies the 32-byte (16-pointer) vector table between the
/// caller's `list` in memory and the system table at $0314.
///
/// * `read_current == true`  (carry set):  copy the live system vectors *out*
///   into `list`.
/// * `read_current == false` (carry clear): install `list` *into* the system
///   vectors.
pub fn vector(m: &mut C64Mem, list: u16, read_current: bool) {
    // LDY #$1F down to 0 — 16 little-endian pointers = 32 bytes.
    for y in (0..=0x1Fu16).rev() {
        let a = if read_current {
            m.r(CINV + y) // LDA $0314,Y   (BCS skips the next load)
        } else {
            m.r(list + y) // LDA ($C3),Y
        };
        m.w(list + y, a); // STA ($C3),Y
        m.w(CINV + y, a); // STA $0314,Y
    }
}

/// RESTOR ($FD15). Installs the KERNAL's default vectors. In the ROM the
/// defaults live at $FD30; here you pass the 32-byte default table so the
/// routine stays self-contained.
pub fn restor(m: &mut C64Mem, default_table: &[u8; 32]) {
    // Stage the defaults where VECTOR expects the caller's list, then install.
    // (The ROM points $C3 straight at the in-ROM $FD30 copy.)
    const STAGING: u16 = 0x0334; // just past the vector table, scratch
    for (i, &b) in default_table.iter().enumerate() {
        m.ram[STAGING as usize + i] = b;
    }
    vector(m, STAGING, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restor_installs_defaults_then_vector_reads_them_back() {
        let mut m = C64Mem::new();
        // A recognizable default table (16 pointers).
        let mut defaults = [0u8; 32];
        for (i, b) in defaults.iter_mut().enumerate() {
            *b = 0x40 + i as u8;
        }
        restor(&mut m, &defaults);

        // The system table at $0314 now holds the defaults.
        for i in 0..32u16 {
            assert_eq!(m.r(CINV + i), 0x40 + i as u8);
        }

        // A program overwrites the live IRQ vector ($0314/$0315)...
        m.w(CINV, 0x00);
        m.w(CINV + 1, 0xC0); // point IRQ at $C000
                             // ...and reads the whole table back out into its own buffer.
        let buf = 0x2000u16;
        vector(&mut m, buf, true);
        assert_eq!(m.r(buf), 0x00);
        assert_eq!(m.r(buf + 1), 0xC0);
        assert_eq!(m.r(buf + 2), 0x42); // unchanged entry
    }
}
