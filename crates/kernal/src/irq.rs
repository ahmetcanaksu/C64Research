//! The default IRQ handler ($EA31) and keyboard scan ($EA87).
//!
//! Once RESET enables interrupts, CIA1 Timer A fires ~60 times a second and,
//! through the RAM vector at $0314 (whose default is $EA31), runs this handler.
//! It is the machine's heartbeat: it advances the clock, blinks the cursor,
//! services the cassette motor, and scans the keyboard.
//!
//! Original ROM listing of the handler:
//!
//! ```text
//! EA31:  20 EA FF  JSR  $FFEA        ; UDTIM: jiffy clock + STOP key
//! EA34:  A5 CC     LDA  $CC          ; cursor-enable flag (0 = enabled)
//! EA36:  D0 29     BNE  $EA61        ; disabled -> skip cursor
//! EA38:  C6 CD     DEC  $CD          ; blink countdown
//! EA3A:  D0 25     BNE  $EA61
//! EA3C:  A9 14     LDA  #$14         ; reload countdown = 20 jiffies
//! EA3E:  85 CD     STA  $CD
//! EA40:  A4 D3     LDY  $D3          ; cursor column
//! EA42:  46 CF     LSR  $CF          ; blink phase -> carry
//! EA44:  AE 87 02  LDX  $0287        ; saved colour under cursor
//! EA47:  B1 D1     LDA  ($D1),Y      ; character under cursor
//! EA49:  B0 11     BCS  $EA5C        ; phase already on -> just toggle back
//! EA4B:  E6 CF     INC  $CF          ; phase = on
//! EA4D:  85 CE     STA  $CE          ; save the real character
//! EA4F:  20 24 EA  JSR  $EA24        ; point $F3 at this line's colour RAM
//! EA52:  B1 F3     LDA  ($F3),Y      ; save the colour under the cursor
//! EA54:  8D 87 02  STA  $0287
//! EA57:  AE 86 02  LDX  $0286        ; use current text colour for the block
//! EA5A:  A5 CE     LDA  $CE
//! EA5C:  49 80     EOR  #$80         ; flip reverse-video bit
//! EA5E:  20 1C EA  JSR  $EA1C        ; write char + colour to the cursor cell
//! EA61:  A5 01 ... (cassette motor control from the 6510 port bit 4) ... STA $01
//! EA7B:  20 87 EA  JSR  $EA87        ; SCNKEY: scan the keyboard
//! EA7E:  AD 0D DC  LDA  $DC0D        ; acknowledge the CIA1 interrupt
//! EA81:  68/A8/68/AA/68/40          ; pull Y,X,A and RTI
//! ```

use crate::C64Mem;

/// Zero-page and register locations used by the IRQ handler / keyboard scan.
mod loc {
    pub const CURSOR_ENABLE: u16 = 0x00CC; // 0 = cursor on
    pub const BLINK_COUNTDOWN: u16 = 0x00CD;
    pub const BLINK_PHASE: u16 = 0x00CF; // bit0: is the block currently shown?
    pub const CHAR_UNDER_CURSOR: u16 = 0x00CE;
    pub const CURSOR_COL: u16 = 0x00D3;
    pub const LINE_PTR: u16 = 0x00D1; // ($D1) -> current screen line
    pub const COLOR_PTR: u16 = 0x00F3; // ($F3) -> current colour-RAM line
    pub const SAVED_COLOR: u16 = 0x0287;
    pub const TEXT_COLOR: u16 = 0x0286;

    pub const CIA1_PRA: u16 = 0xDC00; // keyboard rows (output)
    pub const CIA1_PRB: u16 = 0xDC01; // keyboard columns (input)
    pub const MODIFIERS: u16 = 0x028D; // bit0 SHIFT, bit1 C=, bit2 CTRL
    pub const CUR_KEY: u16 = 0x00CB; // matrix index 0..64 of the pressed key
    pub const LAST_KEY: u16 = 0x00C5; // for debounce
    pub const KBD_BUFFER: u16 = 0x0277; // 10-byte type-ahead buffer
    pub const KBD_COUNT: u16 = 0x00C6; // bytes currently in the buffer
    pub const KBD_BUFFER_MAX: u16 = 0x0289; // configured max (10)
    pub const VIC_MEMPTR: u16 = 0xD018; // char-set select (SHIFT+C= toggles case)
}

/// The four keyboard decode tables (65 entries each, ROM $EB81/$EBC2/$EC03/
/// $EC78). Index by matrix key (0..63) to get the PETSCII code; the modifier
/// bits in `$028D` pick the table. `$FF` marks "no key / unused".
pub const UNSHIFTED_KEYS: [u8; 65] = [
    0x14, 0x0D, 0x1D, 0x88, 0x85, 0x86, 0x87, 0x11, 0x33, 0x57, 0x41, 0x34, 0x5A, 0x53, 0x45, 0x01,
    0x35, 0x52, 0x44, 0x36, 0x43, 0x46, 0x54, 0x58, 0x37, 0x59, 0x47, 0x38, 0x42, 0x48, 0x55, 0x56,
    0x39, 0x49, 0x4A, 0x30, 0x4D, 0x4B, 0x4F, 0x4E, 0x2B, 0x50, 0x4C, 0x2D, 0x2E, 0x3A, 0x40, 0x2C,
    0x5C, 0x2A, 0x3B, 0x13, 0x01, 0x3D, 0x5E, 0x2F, 0x31, 0x5F, 0x04, 0x32, 0x20, 0x02, 0x51, 0x03,
    0xFF,
];
pub const SHIFTED_KEYS: [u8; 65] = [
    0x94, 0x8D, 0x9D, 0x8C, 0x89, 0x8A, 0x8B, 0x91, 0x23, 0xD7, 0xC1, 0x24, 0xDA, 0xD3, 0xC5, 0x01,
    0x25, 0xD2, 0xC4, 0x26, 0xC3, 0xC6, 0xD4, 0xD8, 0x27, 0xD9, 0xC7, 0x28, 0xC2, 0xC8, 0xD5, 0xD6,
    0x29, 0xC9, 0xCA, 0x30, 0xCD, 0xCB, 0xCF, 0xCE, 0xDB, 0xD0, 0xCC, 0xDD, 0x3E, 0x5B, 0xBA, 0x3C,
    0xA9, 0xC0, 0x5D, 0x93, 0x01, 0x3D, 0xDE, 0x3F, 0x21, 0x5F, 0x04, 0x22, 0xA0, 0x02, 0xD1, 0x83,
    0xFF,
];
pub const CBM_KEYS: [u8; 65] = [
    0x94, 0x8D, 0x9D, 0x8C, 0x89, 0x8A, 0x8B, 0x91, 0x96, 0xB3, 0xB0, 0x97, 0xAD, 0xAE, 0xB1, 0x01,
    0x98, 0xB2, 0xAC, 0x99, 0xBC, 0xBB, 0xA3, 0xBD, 0x9A, 0xB7, 0xA5, 0x9B, 0xBF, 0xB4, 0xB8, 0xBE,
    0x29, 0xA2, 0xB5, 0x30, 0xA7, 0xA1, 0xB9, 0xAA, 0xA6, 0xAF, 0xB6, 0xDC, 0x3E, 0x5B, 0xA4, 0x3C,
    0xA8, 0xDF, 0x5D, 0x93, 0x01, 0x3D, 0xDE, 0x3F, 0x81, 0x5F, 0x04, 0x95, 0xA0, 0x02, 0xAB, 0x83,
    0xFF,
];
pub const CTRL_KEYS: [u8; 65] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x1C, 0x17, 0x01, 0x9F, 0x1A, 0x13, 0x05, 0xFF,
    0x9C, 0x12, 0x04, 0x1E, 0x03, 0x06, 0x14, 0x18, 0x1F, 0x19, 0x07, 0x9E, 0x02, 0x08, 0x15, 0x16,
    0x12, 0x09, 0x0A, 0x92, 0x0D, 0x0B, 0x0F, 0x0E, 0xFF, 0x10, 0x0C, 0xFF, 0xFF, 0x1B, 0x00, 0xFF,
    0x1C, 0xFF, 0x1D, 0xFF, 0xFF, 0x1F, 0x1E, 0xFF, 0x90, 0x06, 0xFF, 0x05, 0xFF, 0xFF, 0x11, 0xFF,
    0xFF,
];

/// The default IRQ handler ($EA31). One 60 Hz tick of housekeeping.
pub fn irq_handler(m: &mut C64Mem) {
    update_time_and_stop_key(m); // JSR $FFEA -> UDTIM
    blink_cursor(m); // $EA34..$EA60
    cassette_motor(m); // $EA61..$EA7A
    scnkey(m); // JSR $EA87
                // $EA7E: read $DC0D to acknowledge the CIA interrupt, then RTI.
    let _ = m.r(0xDC0D);
}

fn update_time_and_stop_key(m: &mut C64Mem) {
    crate::udtim::udtim(m);
}

/// Blink the cursor: every 20 ticks, toggle the reverse-video block on/off,
/// preserving the real character and colour beneath it ($EA34..$EA60).
fn blink_cursor(m: &mut C64Mem) {
    if m.r(loc::CURSOR_ENABLE) != 0 {
        return; // cursor disabled
    }
    let n = m.r(loc::BLINK_COUNTDOWN).wrapping_sub(1);
    m.w(loc::BLINK_COUNTDOWN, n);
    if n != 0 {
        return; // not time to toggle yet
    }
    m.w(loc::BLINK_COUNTDOWN, 0x14); // reload = 20

    let y = m.r(loc::CURSOR_COL);
    // LSR $CF: the old phase bit0 goes into "carry"; the block is shown now iff
    // that bit was set.
    let phase = m.r(loc::BLINK_PHASE);
    let showing = phase & 1 != 0;
    m.w(loc::BLINK_PHASE, phase >> 1);

    let line = m.zp_ptr(loc::LINE_PTR as u8);
    let ch = m.r(line.wrapping_add(y as u16)); // char currently on screen

    let (ch, color) = if showing {
        // Block is already shown: put the real character back.
        (ch, m.r(loc::TEXT_COLOR))
    } else {
        // Turn the block on: remember the real char + colour first.
        m.w(loc::BLINK_PHASE, m.r(loc::BLINK_PHASE) + 1); // INC $CF (phase = on)
        m.w(loc::CHAR_UNDER_CURSOR, ch);
        color_ptr(m); // point $F3 at colour RAM for this line
        let cy = m.zp_ptr(loc::COLOR_PTR as u8).wrapping_add(y as u16);
        m.w(loc::SAVED_COLOR, m.r(cy));
        (m.r(loc::CHAR_UNDER_CURSOR), m.r(loc::TEXT_COLOR))
    };
    put_at_cursor(m, ch ^ 0x80, color); // EOR #$80 toggles reverse video
}

/// Store char `a` + colour `x` at the cursor cell ($EA1C).
///
/// ```text
/// EA1C: LDY $D3; STA ($D1),Y; TXA; STA ($F3),Y; RTS
/// ```
fn put_at_cursor(m: &mut C64Mem, a: u8, x: u8) {
    let y = m.r(loc::CURSOR_COL) as u16;
    let line = m.zp_ptr(loc::LINE_PTR as u8);
    m.w(line.wrapping_add(y), a);
    let color = m.zp_ptr(loc::COLOR_PTR as u8);
    m.w(color.wrapping_add(y), x);
}

/// Point the colour pointer ($F3) at the colour-RAM row matching the screen row
/// pointer ($D1): same low byte, high byte forced into colour RAM $D8xx ($EA24).
///
/// ```text
/// EA24: LDA $D1; STA $F3; LDA $D2; AND #$03; ORA #$D8; STA $F4; RTS
/// ```
fn color_ptr(m: &mut C64Mem) {
    m.w(loc::COLOR_PTR, m.r(loc::LINE_PTR));
    let hi = (m.r(loc::LINE_PTR + 1) & 0x03) | 0xD8;
    m.w(loc::COLOR_PTR + 1, hi);
}

/// Cassette motor control from the 6510 port ($EA61..$EA7A). With no Datassette
/// this just keeps the motor line consistent; modeled for faithfulness.
fn cassette_motor(m: &mut C64Mem) {
    let port = m.r(0x0001);
    if port & 0x10 == 0 {
        // Play button pressed: clear the interlock and turn the motor on.
        m.w(0x00C0, 0x00);
        m.w(0x0001, port | 0x20);
    } else if m.r(0x00C0) == 0 {
        // No button: force the motor off.
        m.w(0x0001, m.r(0x0001) & 0x1F);
    }
}

/// SCNKEY ($EA87) — scan the 8x8 keyboard matrix, decode the pressed key to
/// PETSCII, and push it into the type-ahead buffer.
///
/// ```text
/// EA87: clear $028D (modifiers); default key index = $40 (none)
///       for each of 8 rows: drive one CIA1 port-A line low ($FE,$FD,...),
///         read the 8 column bits on port B; for each low bit (pressed):
///           look up its unshifted code; codes < 5 (except 3) are the modifier
///           keys SHIFT/C=/CTRL -> OR into $028D; any other key -> its index.
///       (EB48) pick a decode table from the modifier bits, look up PETSCII,
///       and (with debounce against the last key) store it in the buffer.
/// ```
pub fn scnkey(m: &mut C64Mem) {
    m.w(loc::MODIFIERS, 0x00);
    let mut key_index: u8 = 0x40; // 64 = "no key"

    let mut row_select: u8 = 0xFE; // walking zero across the 8 keyboard rows
    let mut idx: u8 = 0;
    for _ in 0..8 {
        m.w(loc::CIA1_PRA, row_select);
        let mut cols = m.r(loc::CIA1_PRB);
        for _ in 0..8 {
            if cols & 1 == 0 {
                // key at this matrix position is held down
                let code = UNSHIFTED_KEYS[idx as usize];
                if code < 5 && code != 3 {
                    m.w(loc::MODIFIERS, m.r(loc::MODIFIERS) | code); // SHIFT/C=/CTRL
                } else {
                    key_index = idx; // last ordinary key wins
                }
            }
            cols >>= 1;
            idx += 1;
        }
        row_select = (row_select << 1) | 1; // SEC; ROL A -> next row
    }
    m.w(loc::CUR_KEY, key_index);

    decode_and_buffer(m, key_index);
}

/// The $EB48 decode step: choose a table from the modifier bits, translate the
/// key index to PETSCII, and add it to the keyboard buffer (with simple
/// debounce). SHIFT+C= toggles the character set instead of typing.
fn decode_and_buffer(m: &mut C64Mem, key_index: u8) {
    let mods = m.r(loc::MODIFIERS);

    // SHIFT+C= (both bits) toggles upper/lower case via the VIC char pointer.
    if mods == 0x03 {
        m.w(loc::VIC_MEMPTR, m.r(loc::VIC_MEMPTR) ^ 0x02);
        m.w(loc::LAST_KEY, key_index);
        return;
    }

    let table: &[u8; 65] = match mods {
        0x01 => &SHIFTED_KEYS,
        0x02 => &CBM_KEYS,
        0x04 => &CTRL_KEYS,
        _ => &UNSHIFTED_KEYS,
    };

    // Debounce: only act when the key differs from last tick's key.
    let last = m.r(loc::LAST_KEY);
    m.w(loc::LAST_KEY, key_index);
    if key_index == last || key_index >= 64 {
        return;
    }

    let petscii = table[key_index as usize];
    if petscii == 0xFF {
        return; // unused matrix slot
    }

    // Push into the type-ahead buffer if there's room.
    let count = m.r(loc::KBD_COUNT);
    if (count as u16) < (m.r(loc::KBD_BUFFER_MAX) as u16) {
        m.w(loc::KBD_BUFFER + count as u16, petscii);
        m.w(loc::KBD_COUNT, count + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{STKEY, TIME_LO};

    /// A machine set up the way the editor state is just after CINT, with no
    /// keys held (keyboard columns read $FF).
    fn ready() -> C64Mem {
        let mut m = C64Mem::new();
        m.w(loc::CIA1_PRB, 0xFF); // no key pressed
        m.w(0x0289, 0x0A); // buffer max = 10
        m.w(loc::CURSOR_ENABLE, 0x00); // cursor enabled
        m.w(loc::BLINK_COUNTDOWN, 0x01); // toggles on the next tick
        m.w(loc::LINE_PTR, 0x00); // cursor line -> $0400
        m.w(loc::LINE_PTR + 1, 0x04);
        m.w(loc::TEXT_COLOR, 0x0E);
        m.w(loc::LAST_KEY, 0x40);
        m
    }

    #[test]
    fn irq_advances_the_jiffy_clock() {
        let mut m = ready();
        let before = m.r(TIME_LO);
        irq_handler(&mut m);
        assert_eq!(m.r(TIME_LO), before.wrapping_add(1));
        // No key held -> STOP key latch reads the empty column ($FF).
        assert_eq!(m.r(STKEY), 0xFF);
    }

    #[test]
    fn cursor_blink_draws_reverse_block() {
        let mut m = ready();
        m.w(0x0400, 0x01); // an 'A' screen code at the cursor
        irq_handler(&mut m);
        // Block turned on: the cell now shows the reverse-video version.
        assert_eq!(m.r(0x0400), 0x01 ^ 0x80);
    }

    #[test]
    fn decoding_key_index_62_types_q() {
        // Matrix index 62 is 'Q'; unshifted it decodes to PETSCII $51.
        let mut m = ready();
        decode_and_buffer(&mut m, 62);
        assert_eq!(m.r(loc::KBD_COUNT), 1);
        assert_eq!(m.r(loc::KBD_BUFFER), 0x51); // 'Q'
    }

    #[test]
    fn shifted_key_uses_the_shift_table() {
        let mut m = ready();
        m.w(loc::MODIFIERS, 0x01); // SHIFT held
        m.w(loc::LAST_KEY, 0x40);
        decode_and_buffer(&mut m, 8); // index 8: unshifted '3' / shifted '#'
        assert_eq!(m.r(loc::KBD_BUFFER), 0x23); // '#'
    }

    #[test]
    fn debounce_suppresses_a_held_key() {
        let mut m = ready();
        decode_and_buffer(&mut m, 62); // first press -> buffered
        decode_and_buffer(&mut m, 62); // still held -> ignored
        assert_eq!(m.r(loc::KBD_COUNT), 1);
    }
}
