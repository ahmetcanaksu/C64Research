//! Zero-page RAM test — the first thing the 1541 runs after reset.
//!
//! Reached from the reset vector: `RESET ($FFFC) -> $EAA0`, which sets up the
//! stack and then falls into this test at `$EAA7`. For each of the 256 zero-page
//! locations it proves the byte can (a) hold its own address, (b) survive 256
//! increments wrapping cleanly back to the start value, and (c) hold zero. Any
//! failure jumps to the fault handler at `$EA6E` (which flashes the drive LED).
//!
//! Original ROM listing (from the traced disassembly of `C1541.rom`):
//!
//! ```text
//! EAA7:  E8        INX            ; X = 0 (was $FF after reset)
//! EAA8:  A0 00     LDY  #$00
//! EAAA:  A2 00     LDX  #$00
//! ; fill: zp[X] = X for all 256 bytes
//! EAAC:  8A        TXA
//! EAAD:  95 00     STA  $00,X
//! EAAF:  E8        INX
//! EAB0:  D0 FA     BNE  $EAAC
//! ; per-byte test (X = 0..=255)
//! EAB2:  8A        TXA            ; A = X (expected value)
//! EAB3:  D5 00     CMP  $00,X     ; still holds its index?
//! EAB5:  D0 B7     BNE  $EA6E     ; no -> RAM fault
//! EAB7:  F6 00     INC  $00,X     ; 256 increments must wrap back to X
//! EAB9:  C8        INY
//! EABA:  D0 FB     BNE  $EAB7
//! EABC:  D5 00     CMP  $00,X     ; wrapped back to X?
//! EABE:  D0 AE     BNE  $EA6E     ; no -> RAM fault
//! EAC0:  94 00     STY  $00,X     ; write 0 (Y == 0 here)
//! EAC2:  B5 00     LDA  $00,X
//! EAC4:  D0 A8     BNE  $EA6E     ; must read back 0
//! EAC6:  E8        INX
//! EAC7:  D0 E9     BNE  $EAB2     ; next byte
//! ```

/// Byte-addressable zero page ($00–$FF). Implemented by real RAM
/// (`[u8; 256]`) and, in tests, by models of faulty silicon.
pub trait ZeroPage {
    fn read(&self, addr: u8) -> u8;
    fn write(&mut self, addr: u8, val: u8);
}

impl ZeroPage for [u8; 256] {
    #[inline]
    fn read(&self, addr: u8) -> u8 {
        self[addr as usize]
    }
    #[inline]
    fn write(&mut self, addr: u8, val: u8) {
        self[addr as usize] = val;
    }
}

/// Run the 1541 zero-page RAM test.
///
/// Returns `Ok(())` if every location passes (the ROM would continue booting),
/// or `Err(addr)` with the first failing address (the ROM would branch to its
/// fault handler at `$EA6E`).
pub fn zero_page_ram_test<M: ZeroPage>(zp: &mut M) -> Result<(), u8> {
    // EAAC..EAB0: fill zp[i] = i for all 256 bytes.
    for i in 0u16..256 {
        zp.write(i as u8, i as u8);
    }

    // EAB2..EAC7: test each byte in turn.
    for x in 0u16..256 {
        let x = x as u8;

        // EAB3/EAB5: still holds its own index?
        if zp.read(x) != x {
            return Err(x);
        }

        // EAB7..EABA: 256 increments (Y wraps 0->0) must return to the start.
        for _ in 0u16..256 {
            let v = zp.read(x).wrapping_add(1);
            zp.write(x, v);
        }
        // EABC/EABE: wrapped cleanly back to x?
        if zp.read(x) != x {
            return Err(x);
        }

        // EAC0..EAC4: write 0 and read it back.
        zp.write(x, 0);
        if zp.read(x) != 0 {
            return Err(x);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_ram_passes() {
        let mut zp = [0u8; 256];
        assert_eq!(zero_page_ram_test(&mut zp), Ok(()));
        // Leaves zero page cleared, exactly like the ROM does before booting.
        assert!(zp.iter().all(|&b| b == 0));
    }

    /// RAM where one address has a data bit permanently stuck low — the classic
    /// fault this power-on test exists to catch.
    struct StuckBit {
        cells: [u8; 256],
        bad_addr: u8,
        stuck_mask: u8, // bits forced to 0 on write
    }

    impl ZeroPage for StuckBit {
        fn read(&self, addr: u8) -> u8 {
            self.cells[addr as usize]
        }
        fn write(&mut self, addr: u8, val: u8) {
            let v = if addr == self.bad_addr { val & !self.stuck_mask } else { val };
            self.cells[addr as usize] = v;
        }
    }

    #[test]
    fn stuck_bit_is_detected_at_its_address() {
        // Address $01, bit 0 stuck low. The fill phase writes zp[$01] = $01,
        // but bit 0 can't be set, so it stores $00 and the identity check
        // (CMP $00,X) fails at exactly $01.
        //
        // Note: the same fault at an *even* address (e.g. $80) would slip
        // through, because that index has bit 0 already clear — a real quirk of
        // this test's coverage, not a bug in the port.
        let mut zp = StuckBit { cells: [0; 256], bad_addr: 0x01, stuck_mask: 0x01 };
        assert_eq!(zero_page_ram_test(&mut zp), Err(0x01));
    }

    #[test]
    fn stuck_high_bit_is_detected() {
        // A cell that can never read back 0 fails the final zero check.
        struct StuckHigh([u8; 256]);
        impl ZeroPage for StuckHigh {
            fn read(&self, addr: u8) -> u8 {
                if addr == 0x00 { self.0[0] | 0x40 } else { self.0[addr as usize] }
            }
            fn write(&mut self, addr: u8, val: u8) {
                self.0[addr as usize] = val;
            }
        }
        let mut zp = StuckHigh([0; 256]);
        assert_eq!(zero_page_ram_test(&mut zp), Err(0x00));
    }
}
