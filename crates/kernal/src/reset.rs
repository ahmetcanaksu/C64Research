//! RESET ($FCE2) — the KERNAL's entry point and boot sequence, ported all the
//! way to the hand-off into BASIC.
//!
//! This is "the main function": the CPU's reset vector ($FFFC) points here, and
//! everything the C64 does at power-on happens in this routine and the handful
//! it calls. It ends with `JMP ($A000)` — a jump through the BASIC cold-start
//! vector — which is where the KERNAL's job stops and BASIC takes over.
//!
//! Boot chain (each step is a routine ported below or already in this crate):
//!
//! ```text
//! FCE2:  A2 FF     LDX  #$FF        ; \
//! FCE4:  78        SEI              ;  } set up stack, disable IRQ, binary mode
//! FCE5:  9A        TXS              ; /
//! FCE6:  D8        CLD
//! FCE7:  20 02 FD  JSR  $FD02       ; cartridge_present?  (CBM80 signature)
//! FCEA:  D0 03     BNE  $FCEF       ; no cart -> normal boot
//! FCEC:  6C 00 80  JMP  ($8000)     ; cart present -> its cold-start
//! FCEF:  8E 16 D0  STX  $D016       ; X=$FF -> VIC control register 2
//! FCF2:  20 A3 FD  JSR  $FDA3       ; ioinit    (CIA / SID / VIC bank / timers)
//! FCF5:  20 50 FD  JSR  $FD50       ; ramtas    (RAM test/clear/size)
//! FCF8:  20 15 FD  JSR  $FD15       ; restor    (install RAM I/O vectors)
//! FCFB:  20 5B FF  JSR  $FF5B       ; cint      (VIC + screen editor init)
//! FCFE:  58        CLI              ; enable interrupts
//! FCFF:  6C 00 A0  JMP  ($A000)     ; -> BASIC cold start  (the hand-off)
//! ```

use crate::ramtas::ramtas;
use crate::vectors::restor;
use crate::{C64Mem, SCREEN_PAGE};

/// The "CBM80" autostart signature a cartridge places at $8004-$8008
/// (`C3 C2 CD 38 30` = 'C'|$80,'B'|$80,'M'|$80,'8','0').
pub const CBM80: [u8; 5] = [0xC3, 0xC2, 0xCD, 0x38, 0x30];

/// The KERNAL's 16 default I/O vectors, copied from $FD30 into $0314 by RESTOR.
/// (First entry $EA31 = the default IRQ handler.)
pub const DEFAULT_VECTORS: [u8; 32] = [
    0x31, 0xEA, 0x66, 0xFE, 0x47, 0xFE, 0x4A, 0xF3, 0x91, 0xF2, 0x0E, 0xF2, 0x50, 0xF2, 0x33, 0xF3,
    0x57, 0xF1, 0xCA, 0xF1, 0xED, 0xF6, 0x3E, 0xF1, 0x2F, 0xF3, 0x66, 0xFE, 0xA5, 0xF4, 0xED, 0xF5,
];

/// The VIC-II power-on register values, copied from $ECB9 into $D000-$D02E by
/// `vic_init_from_table`. Notable entries: [$11]=$9B (screen on, 25 rows),
/// [$18]=$14 (screen at $0400, chars at $1000), [$20]=$0E border light-blue,
/// [$21]=$06 background blue.
pub const VIC_INIT_TABLE: [u8; 47] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x9B, 0x37, 0x00, 0x00, 0x00, 0x08, 0x00, 0x14, 0x0F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x0E, 0x06, 0x01, 0x02, 0x03, 0x04, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x4C,
];

/// Read a little-endian word from any address.
fn word(m: &C64Mem, addr: u16) -> u16 {
    u16::from_le_bytes([m.r(addr), m.r(addr.wrapping_add(1))])
}

/// Cartridge check ($FD02). True if a cartridge's "CBM80" autostart signature
/// is present at $8004-$8008.
///
/// ```text
/// FD02:  A2 05     LDX  #$05
/// FD04:  BD 0F FD  LDA  $FD0F,X     ; signature byte
/// FD07:  DD 03 80  CMP  $8003,X     ; vs cartridge ROM
/// FD0A:  D0 03     BNE  $FD0F       ; mismatch -> return (Z=0)
/// FD0C:  CA        DEX
/// FD0D:  D0 F5     BNE  $FD04
/// FD0F:  60        RTS              ; all matched -> return (Z=1)
/// ```
pub fn cartridge_present(m: &C64Mem) -> bool {
    (0..5u16).all(|i| m.r(0x8004 + i) == CBM80[i as usize])
}

/// IOINIT ($FDA3, incl. the $FF6E tail). Brings the two CIAs, the SID, the VIC
/// bank, and the 6510 port to their power-on state, and starts CIA1 Timer A —
/// the ~60 Hz interrupt that drives the jiffy clock, cursor, and keyboard scan.
///
/// ```text
/// FDA3: LDA #$7F; STA $DC0D/$DD0D   ; CIA1/2: clear all interrupt enables
///       STA $DC00                    ; CIA1 port A
///       LDA #$08; STA $DC0E/$DD0E/$DC0F/$DD0F  ; timers stopped, one-shot
///       LDX #$00; STX $DC03/$DD03    ; port B = input
///       STX $D418                    ; SID volume = 0
///       DEX; STX $DC02               ; CIA1 DDRA = $FF (keyboard columns out)
///       LDA #$07; STA $DD00          ; CIA2 A: VIC bank 0 + serial bus
///       LDA #$3F; STA $DD02          ; CIA2 DDRA
///       LDA #$E7; STA $01            ; 6510 port: ROMs+I/O banked in
///       LDA #$2F; STA $00            ; 6510 data direction
///       (PAL/NTSC from $02A6) -> CIA1 Timer A latch $DC04/$DC05
/// FF6E: LDA #$81; STA $DC0D          ; enable Timer A interrupt
///       LDA $DC0E; AND #$80; ORA #$11; STA $DC0E  ; start Timer A, continuous
/// ```
pub fn ioinit(m: &mut C64Mem) {
    m.w(0xDC0D, 0x7F);
    m.w(0xDD0D, 0x7F);
    m.w(0xDC00, 0x7F);
    for r in [0xDC0E, 0xDD0E, 0xDC0F, 0xDD0F] {
        m.w(r, 0x08);
    }
    m.w(0xDC03, 0x00);
    m.w(0xDD03, 0x00);
    m.w(0xD418, 0x00);
    m.w(0xDC02, 0xFF); // DEX made X=$FF
    m.w(0xDD00, 0x07);
    m.w(0xDD02, 0x3F);
    m.w(0x0001, 0xE7); // 6510 I/O port
    m.w(0x0000, 0x2F); // 6510 data-direction

    // CIA1 Timer A latch for a ~60 Hz tick: PAL vs NTSC per the $02A6 flag.
    if m.r(0x02A6) != 0 {
        m.w(0xDC04, 0x25); // PAL: $4025
        m.w(0xDC05, 0x40);
    } else {
        m.w(0xDC04, 0x95); // NTSC: $4295
        m.w(0xDC05, 0x42);
    }

    // $FF6E tail: enable the Timer A interrupt and start the timer.
    m.w(0xDC0D, 0x81);
    let cra = (m.r(0xDC0E) & 0x80) | 0x11;
    m.w(0xDC0E, cra);
}

/// Copy the VIC-II power-on register table to $D000-$D02E ($E5A0). Also sets the
/// default I/O devices (screen out, keyboard in).
///
/// ```text
/// E5A0: LDA #$03; STA $9A            ; default output device = screen
///       LDA #$00; STA $99            ; default input device = keyboard
///       LDX #$2F
/// E5AA: LDA $ECB8,X; STA $CFFF,X; DEX; BNE $E5AA   ; 47 bytes -> $D000..$D02E
/// ```
pub fn vic_init_from_table(m: &mut C64Mem) {
    m.w(0x009A, 0x03);
    m.w(0x0099, 0x00);
    for (i, &b) in VIC_INIT_TABLE.iter().enumerate() {
        m.w(0xD000 + i as u16, b);
    }
}

/// CINT ($E518) — initialize the VIC-II and the screen editor: load VIC
/// registers, set the editor's variables, build the screen line-link table, and
/// clear the screen.
///
/// ```text
/// E518: JSR $E5A0                    ; vic_init_from_table
///       (set editor vars: color=$0E, kbd decode=$EB48, buffer size, cursor...)
/// E544: build $D9-$F2 line-link table: high byte of each screen line's address
///       (start = screen page | $80; +$28 per line, carry into the high byte)
/// E55E: LDX #$18; JSR $E9FF x25      ; clear each of the 25 screen lines
///       (cursor to home)
/// ```
///
/// The per-line clear ($E9FF) is summarized here as its net effect: fill the
/// 1000 screen cells with the space code ($20) and the 1000 color cells with the
/// current text color.
pub fn cint(m: &mut C64Mem) {
    vic_init_from_table(m); // E518: JSR $E5A0

    // Editor variables (E51B..E542), byte for byte.
    m.w(0x0291, 0x00); // charset shift lock
    m.w(0x00CF, 0x00); // cursor blink phase
    m.w(0x028F, 0x48); // keyboard-decode vector = $EB48
    m.w(0x0290, 0xEB);
    m.w(0x0289, 0x0A); // keyboard buffer max size = 10
    m.w(0x028C, 0x0A); // key repeat delay
    m.w(0x0286, 0x0E); // current text color = light blue (14)
    m.w(0x028B, 0x04); // key repeat speed
    m.w(0x00CD, 0x0C); // cursor blink countdown
    m.w(0x00CC, 0x0C); // cursor blink enable

    // Build the screen line-link table at $D9..$F2: the high byte of each
    // line's start address, bit 7 set to mark a (non-wrapped) line start.
    let mut hi = m.r(SCREEN_PAGE) | 0x80; // $04 | $80
    let mut acc: u16 = 0;
    for x in 0..26u16 {
        m.w(0x00D9 + x, hi); // STY $D9,X  (store BEFORE advancing, as the ROM does)
        acc += 0x28; // CLC; ADC #$28
        if acc > 0xFF {
            hi = hi.wrapping_add(1); // INY on carry
            acc -= 0x100;
        }
    }
    m.w(0x00D9 + 26, 0xFF); // table terminator

    // Clear the screen to spaces + current color (net effect of the E9FF loop).
    let screen = (m.r(SCREEN_PAGE) as u16) << 8;
    let color = m.r(0x0286);
    for i in 0..1000u16 {
        m.w(screen + i, 0x20); // space
        m.w(0xD800 + i, color); // color RAM
    }

    // Cursor home.
    m.w(0x00D3, 0x00); // column
    m.w(0x00D6, 0x00); // row
}

/// RESET ($FCE2) — the KERNAL main. Runs the whole boot sequence and returns the
/// address it would `JMP ($A000)` to: the BASIC cold-start entry (the point
/// where the KERNAL hands control to BASIC). If a cartridge is present, returns
/// its cold-start vector from $8000 instead.
pub fn reset(m: &mut C64Mem) -> u16 {
    // LDX #$FF / SEI / TXS / CLD — stack pointer and flags (state we don't model
    // here since these ports operate on memory, not the CPU registers).
    if cartridge_present(m) {
        return word(m, 0x8000); // JMP ($8000)
    }
    m.w(0xD016, 0xFF); // STX $D016 (X = $FF)
    ioinit(m); //   JSR $FDA3
    ramtas(m); //   JSR $FD50   (already ported)
    restor(m, &DEFAULT_VECTORS); // JSR $FD15  (already ported)
    cint(m); //     JSR $FF5B -> $E518
    // CLI; JMP ($A000) — hand off to BASIC. On real hardware $A000 holds the
    // BASIC ROM's cold-start vector ($E394).
    word(m, 0xA000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CINV;

    // Reset presents the machine as the hardware does: reads see BASIC ROM at
    // $A000 (RAM ends there) and its cold-start vector is $E394. See
    // `crate::test_machine_with_rom`.
    use crate::test_machine_with_rom as fresh;

    #[test]
    fn boot_chain_runs_to_basic_cold_start() {
        let mut m = fresh();

        let entry = reset(&mut m);
        assert_eq!(entry, 0xE394, "should hand off to BASIC cold start");

        // IOINIT ran: 6510 port banked, CIA1 Timer A started + IRQ enabled.
        assert_eq!(m.r(0x0001), 0xE7);
        assert_eq!(m.r(0xDC0D), 0x81);
        assert_eq!(m.r(0xDC0E) & 0x11, 0x11);

        // RAMTAS ran: found top of RAM at the ROM boundary.
        assert_eq!(m.r(0x0284), 0xA0); // MEMSIZ high

        // RESTOR ran: default vectors installed ($0314 = IRQ handler $EA31).
        assert_eq!(m.r(CINV), 0x31);
        assert_eq!(m.r(CINV + 1), 0xEA);

        // CINT ran: VIC border/background set, screen cleared to spaces.
        assert_eq!(m.r(0xD020), 0x0E); // border light blue
        assert_eq!(m.r(0xD021), 0x06); // background blue
        assert_eq!(m.r(0x0400), 0x20); // top-left screen cell = space
        assert_eq!(m.r(0xD800), 0x0E); // color RAM = light blue
        assert_eq!(m.r(0x0286), 0x0E); // current text color
    }

    #[test]
    fn line_link_table_matches_screen_rows() {
        let mut m = fresh();
        m.w(SCREEN_PAGE, 0x04); // normally set by RAMTAS before CINT runs
        cint(&mut m);
        // Line 0 starts at $0400 (page $04, high byte $84 with bit 7 set).
        assert_eq!(m.r(0x00D9), 0x84);
        // Line 7 crosses into page $05 ($0400 + 7*40 = $0518).
        assert_eq!(m.r(0x00D9 + 7), 0x85);
    }

    #[test]
    fn cartridge_autostart_is_detected() {
        let mut m = fresh();
        // Plant the CBM80 signature and a cold-start vector at $8000.
        for (i, &b) in CBM80.iter().enumerate() {
            m.ram[0x8004 + i] = b;
        }
        m.ram[0x8000] = 0x00;
        m.ram[0x8001] = 0x80; // cart cold start = $8000
        assert!(cartridge_present(&m));
        assert_eq!(reset(&mut m), 0x8000);
    }
}
