//! RAMTAS ($FD50) — the C64's power-on RAM test, clear, and sizing.
//!
//! Called early from RESET. It clears the low pages, then walks memory upward
//! from $0400 writing a test pattern to find where RAM ends, and records the
//! top of memory plus the default screen/low-memory pointers. It's the C64's
//! cousin of the 1541 zero-page RAM test — same idea (write a value, read it
//! back), applied across all of RAM.
//!
//! Original ROM listing:
//!
//! ```text
//! FD50:  A9 00     LDA  #$00
//! FD52:  A8        TAY
//! ; clear $0002-$0101, $0200-$02FF, $0300-$03FF
//! FD53:  99 02 00  STA  $0002,Y
//! FD56:  99 00 02  STA  $0200,Y
//! FD59:  99 00 03  STA  $0300,Y
//! FD5C:  C8        INY
//! FD5D:  D0 F4     BNE  $FD53
//! FD5F:  A2 3C     LDX  #$3C
//! FD61:  A0 03     LDY  #$03
//! FD63:  86 B2     STX  $B2         ; tape buffer pointer = $033C
//! FD65:  84 B3     STY  $B3
//! FD67:  A8        TAY              ; Y = 0
//! FD68:  A9 03     LDA  #$03
//! FD6A:  85 C2     STA  $C2         ; ($C1) high = 3 ...
//! FD6C:  E6 C2     INC  $C2         ; ... ++ -> start testing at page $04 ($0400)
//! FD6E:  B1 C1     LDA  ($C1),Y     ; save original byte
//! FD70:  AA        TAX
//! FD71:  A9 55     LDA  #$55        ; write %01010101
//! FD73:  91 C1     STA  ($C1),Y
//! FD75:  D1 C1     CMP  ($C1),Y     ; read back?
//! FD77:  D0 0F     BNE  $FD88       ; no -> top of RAM
//! FD79:  2A        ROL  A           ; $55 -> $AA (%10101010)
//! FD7A:  91 C1     STA  ($C1),Y
//! FD7C:  D1 C1     CMP  ($C1),Y     ; read back?
//! FD7E:  D0 08     BNE  $FD88       ; no -> top of RAM
//! FD80:  8A        TXA              ; restore original byte
//! FD81:  91 C1     STA  ($C1),Y
//! FD83:  C8        INY
//! FD84:  D0 E8     BNE  $FD6E       ; next byte in page
//! FD86:  F0 E4     BEQ  $FD6C       ; page done -> next page
//! ; top of RAM found: X = low (=Y), Y = high (=$C2)
//! FD88:  98        TYA
//! FD89:  AA        TAX
//! FD8A:  A4 C2     LDY  $C2
//! FD8C:  18        CLC
//! FD8D:  20 2D FE  JSR  $FE2D       ; SETTOP: MEMSIZ = X/Y
//! FD90:  A9 08     LDA  #$08
//! FD92:  8D 82 02  STA  $0282       ; start-of-memory page = $08 ($0800)
//! FD95:  A9 04     LDA  #$04
//! FD97:  8D 88 02  STA  $0288       ; screen page = $04 ($0400)
//! FD9A:  60        RTS
//! ```

use crate::{C64Mem, MEMSIZ_HI, MEMSIZ_LO, MEMSTR_HI, SCREEN_PAGE};

/// Run RAMTAS. Returns the detected top-of-RAM address (the first location that
/// wouldn't hold the test pattern). Also writes the KERNAL's memory pointers.
pub fn ramtas(m: &mut C64Mem) -> u16 {
    // Clear $0002-$0101, $0200-$02FF, $0300-$03FF (three indexed stores, Y=0..255).
    for y in 0..=0xFFu16 {
        m.w(0x0002 + y, 0);
        m.w(0x0200 + y, 0);
        m.w(0x0300 + y, 0);
    }

    // Tape buffer pointer $B2/$B3 = $033C.
    m.w(0x00B2, 0x3C);
    m.w(0x00B3, 0x03);

    // Walk RAM from $0400 upward, non-destructively testing each byte.
    let top = 'scan: loop {
        let mut addr: u16 = 0x0400;
        loop {
            let original = m.r(addr);
            m.w(addr, 0x55);
            if m.r(addr) != 0x55 {
                break 'scan addr; // pattern didn't stick -> end of RAM
            }
            m.w(addr, 0xAA); // ROL of $55
            if m.r(addr) != 0xAA {
                break 'scan addr;
            }
            m.w(addr, original); // restore
            match addr.checked_add(1) {
                Some(next) => addr = next,
                None => break 'scan 0x0000, // scanned the whole 64 KB
            }
        }
    };

    // SETTOP: MEMSIZ ($0283/$0284) = top-of-RAM.
    m.w(MEMSIZ_LO, top as u8);
    m.w(MEMSIZ_HI, (top >> 8) as u8);
    // OS start-of-memory page ($0282 -> $0800) and screen page ($0288 -> $0400).
    m.w(MEMSTR_HI, 0x08);
    m.w(SCREEN_PAGE, 0x04);
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_top_of_ram_at_the_rom_boundary() {
        // A machine whose reads see BASIC ROM at $A000 (so RAM ends there).
        let mut m = crate::test_machine_with_rom();
        // Dirty some low memory to prove it gets cleared.
        m.ram[0x0002] = 0xFF;
        m.ram[0x02FE] = 0xFF;
        m.ram[0x0333] = 0xFF;

        let top = ramtas(&mut m);

        assert_eq!(top, 0xA000);
        assert_eq!(m.r(MEMSIZ_LO), 0x00);
        assert_eq!(m.r(MEMSIZ_HI), 0xA0);
        assert_eq!(m.r(MEMSTR_HI), 0x08); // $0800
        assert_eq!(m.r(SCREEN_PAGE), 0x04); // $0400
        // Low pages cleared.
        assert_eq!(m.r(0x0002), 0x00);
        assert_eq!(m.r(0x02FE), 0x00);
        assert_eq!(m.r(0x0333), 0x00);
    }

    #[test]
    fn non_destructive_below_the_top() {
        let mut m = crate::test_machine_with_rom();
        // A byte inside the scanned region must survive (saved and restored).
        m.ram[0x1234] = 0x42;
        ramtas(&mut m);
        assert_eq!(m.r(0x1234), 0x42);
    }
}
