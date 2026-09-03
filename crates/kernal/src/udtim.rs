//! UDTIM ($F69B) — update the jiffy clock and scan the STOP key.
//!
//! Called 60 times a second from the KERNAL's IRQ handler. It bumps the 24-bit
//! TIME counter (wrapping after 24 hours) and takes one debounced sample of the
//! STOP key, latching it into STKEY ($91) where the rest of the OS checks it.
//!
//! Original ROM listing:
//!
//! ```text
//! F69B:  A2 00     LDX  #$00
//! ; ++TIME (three bytes: $A0 high, $A1 mid, $A2 low)
//! F69D:  E6 A2     INC  $A2
//! F69F:  D0 06     BNE  $F6A7
//! F6A1:  E6 A1     INC  $A1
//! F6A3:  D0 02     BNE  $F6A7
//! F6A5:  E6 A0     INC  $A0
//! ; if TIME >= $4F1A01 (jiffies per day) reset it to 0
//! F6A7:  38        SEC
//! F6A8:  A5 A2     LDA  $A2
//! F6AA:  E9 01     SBC  #$01
//! F6AC:  A5 A1     LDA  $A1
//! F6AE:  E9 1A     SBC  #$1A
//! F6B0:  A5 A0     LDA  $A0
//! F6B2:  E9 4F     SBC  #$4F
//! F6B4:  90 06     BCC  $F6BC       ; TIME < limit -> keep
//! F6B6:  86 A0     STX  $A0         ; else zero all three (X = 0)
//! F6B8:  86 A1     STX  $A1
//! F6BA:  86 A2     STX  $A2
//! ; STOP-key scan (debounced reads of CIA1 port B)
//! F6BC:  AD 01 DC  LDA  $DC01
//! F6BF:  CD 01 DC  CMP  $DC01
//! F6C2:  D0 F8     BNE  $F6BC       ; wait for two equal reads
//! F6C4:  AA        TAX
//! F6C5:  30 13     BMI  $F6DA       ; bit 7 set -> just latch it
//! F6C7:  A2 BD     LDX  #$BD
//! F6C9:  8E 00 DC  STX  $DC00       ; select the STOP-key row on port A
//! F6CC:  AE 01 DC  LDX  $DC01
//! F6CF:  EC 01 DC  CPX  $DC01
//! F6D2:  D0 F8     BNE  $F6CC       ; debounce again
//! F6D4:  8D 00 DC  STA  $DC00       ; restore port A
//! F6D7:  E8        INX
//! F6D8:  D0 02     BNE  $F6DC       ; second read != $FF -> don't latch
//! F6DA:  85 91     STA  $91         ; STKEY = first sample
//! F6DC:  60        RTS
//! ```

use crate::{C64Mem, CIA1_PRA, CIA1_PRB, STKEY, TIME_HI, TIME_LO, TIME_MID};

/// Jiffies in a day on the C64 (`$4F1A01` = 5,183,489). The clock resets to
/// zero once it reaches this.
pub const JIFFIES_PER_DAY: u32 = 0x004F_1A01;

pub fn udtim(m: &mut C64Mem) {
    // --- advance TIME (INC $A2, carry into $A1, carry into $A0) ---
    let lo = m.r(TIME_LO).wrapping_add(1);
    m.w(TIME_LO, lo);
    if lo == 0 {
        let mid = m.r(TIME_MID).wrapping_add(1);
        m.w(TIME_MID, mid);
        if mid == 0 {
            m.w(TIME_HI, m.r(TIME_HI).wrapping_add(1));
        }
    }

    // --- wrap after a full day (the ROM's 3-byte SEC/SBC compare) ---
    let time = ((m.r(TIME_HI) as u32) << 16) | ((m.r(TIME_MID) as u32) << 8) | m.r(TIME_LO) as u32;
    if time >= JIFFIES_PER_DAY {
        m.w(TIME_HI, 0);
        m.w(TIME_MID, 0);
        m.w(TIME_LO, 0);
    }

    // --- STOP-key scan: one debounced sample, latched into STKEY ---
    let col = debounced_read(m, CIA1_PRB);
    if col & 0x80 != 0 {
        // bit 7 set (BMI taken): latch the sample directly.
        m.w(STKEY, col);
    } else {
        // Drive the STOP-key row low and re-read; only latch if the second,
        // row-selected read comes back all-ones ($FF).
        m.w(CIA1_PRA, 0xBD);
        let second = debounced_read(m, CIA1_PRB);
        m.w(CIA1_PRA, col); // restore port A (ROM: STA $DC00, A = first sample)
        if second == 0xFF {
            m.w(STKEY, col);
        }
    }
}

/// Read `addr` until two consecutive reads agree — the ROM's `LDA;CMP;BNE`
/// debounce of a noisy keyboard-matrix line.
fn debounced_read(m: &C64Mem, addr: u16) -> u8 {
    loop {
        let a = m.r(addr);
        if a == m.r(addr) {
            return a;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_time(m: &mut C64Mem, v: u32) {
        m.w(TIME_HI, (v >> 16) as u8);
        m.w(TIME_MID, (v >> 8) as u8);
        m.w(TIME_LO, v as u8);
    }
    fn get_time(m: &C64Mem) -> u32 {
        ((m.r(TIME_HI) as u32) << 16) | ((m.r(TIME_MID) as u32) << 8) | m.r(TIME_LO) as u32
    }

    #[test]
    fn clock_increments_with_carry() {
        let mut m = C64Mem::new();
        m.w(CIA1_PRB, 0xFF); // no keys pressed
        set_time(&mut m, 0x00_00FF);
        udtim(&mut m);
        assert_eq!(get_time(&m), 0x00_0100); // carried from low into middle byte
    }

    #[test]
    fn clock_wraps_after_a_day() {
        let mut m = C64Mem::new();
        m.w(CIA1_PRB, 0xFF);
        set_time(&mut m, JIFFIES_PER_DAY - 1); // one tick short of a day
        udtim(&mut m);
        assert_eq!(get_time(&m), 0); // hit the limit -> reset to 0
    }

    #[test]
    fn stop_key_column_is_latched() {
        let mut m = C64Mem::new();
        set_time(&mut m, 0);
        // A pressed key pulls a column bit low; bit 7 set routes straight to the
        // latch. $BF = STOP-key column pattern with bit 7 high.
        m.w(CIA1_PRB, 0xBF);
        udtim(&mut m);
        assert_eq!(m.r(STKEY), 0xBF);
    }
}
