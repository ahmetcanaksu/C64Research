//! MOS 6526 CIA (Complex Interface Adapter).
//!
//! The C64 has two: CIA1 ($DC00) drives the keyboard matrix, joysticks, and the
//! ~60 Hz Timer-A interrupt (wired to the CPU's IRQ); CIA2 ($DD00) drives the
//! serial bus, user port, VIC bank select, and the RESTORE-key NMI.
//!
//! This models what the KERNAL needs to boot and run: two I/O ports with data
//! direction, two interval timers (A and B) with the interrupt logic
//! (ICR/mask), and the read-clears-flags behavior programs rely on. The
//! time-of-day clock and the serial shift register are stubbed (the boot path
//! doesn't drive them).
//!
//! `no_std`, MCU-ready.

#![no_std]

// Register offsets 0x0..0xF.
const PRA: u8 = 0x0;
const PRB: u8 = 0x1;
const DDRA: u8 = 0x2;
const DDRB: u8 = 0x3;
const TA_LO: u8 = 0x4;
const TA_HI: u8 = 0x5;
const TB_LO: u8 = 0x6;
const TB_HI: u8 = 0x7;
const TOD_10TH: u8 = 0x8;
const TOD_SEC: u8 = 0x9;
const TOD_MIN: u8 = 0xA;
const TOD_HR: u8 = 0xB;
const SDR: u8 = 0xC;
const ICR: u8 = 0xD;
const CRA: u8 = 0xE;
const CRB: u8 = 0xF;

// ICR bits.
const INT_TA: u8 = 0x01;
const INT_TB: u8 = 0x02;
const INT_FLAG: u8 = 0x80; // "an enabled interrupt occurred"

/// One 6526 CIA.
#[derive(Debug, Clone)]
pub struct Cia {
    pra: u8,
    prb: u8,
    ddra: u8,
    ddrb: u8,
    /// Levels on the port input pins (open lines read high). Set these to feed
    /// the keyboard/joystick state in.
    pub pa_in: u8,
    pub pb_in: u8,

    ta: u16,
    tb: u16,
    ta_latch: u16,
    tb_latch: u16,
    cra: u8,
    crb: u8,

    icr_data: u8, // pending interrupt flags (bits 0..4)
    icr_mask: u8, // which flags actually assert the IRQ line

    /// Time-of-day clock, BCD: [tenths, seconds, minutes, hours]. The hours byte
    /// carries the AM/PM flag in bit 7.
    tod: [u8; 4],
    tod_latch: [u8; 4], // frozen snapshot returned to reads while latched
    tod_latched: bool,  // reading the hours latches; reading tenths unlatches
    tod_alarm: [u8; 4],
    tod_running: bool, // stopped between writing hours and writing tenths
    tod_acc: u32,      // phi2 cycles toward the next tenth-of-a-second
    sdr: u8,
}

/// PAL phi2 (985248 Hz) / 10: the TOD advances a tenth this often. The chip is
/// really driven by a 50/60 Hz pin; ten ticks a second is what matters here.
const TOD_CYCLES_PER_TENTH: u32 = 98_525;

impl Default for Cia {
    fn default() -> Self {
        Cia {
            pra: 0,
            prb: 0,
            ddra: 0,
            ddrb: 0,
            pa_in: 0xFF,
            pb_in: 0xFF,
            ta: 0xFFFF,
            tb: 0xFFFF,
            ta_latch: 0xFFFF,
            tb_latch: 0xFFFF,
            cra: 0,
            crb: 0,
            icr_data: 0,
            icr_mask: 0,
            tod: [0, 0, 0, 1], // 12-hour clock starts at 1:00:00.0 AM
            tod_latch: [0, 0, 0, 1],
            tod_latched: false,
            tod_alarm: [0; 4],
            tod_running: true,
            tod_acc: 0,
            sdr: 0,
        }
    }
}

/// Increment a BCD byte with a wrap limit; returns `true` on carry.
fn bcd_inc(v: &mut u8, limit: u8) -> bool {
    let mut lo = (*v & 0x0F) + 1;
    let mut hi = *v >> 4;
    if lo > 9 {
        lo = 0;
        hi += 1;
    }
    let next = (hi << 4) | lo;
    if next >= limit {
        *v = 0;
        true
    } else {
        *v = next;
        false
    }
}

impl Cia {
    pub fn new() -> Self {
        Self::default()
    }

    /// Level on the port A pins (output bits from PRA, input bits from pins).
    pub fn port_a(&self) -> u8 {
        (self.pra & self.ddra) | (self.pa_in & !self.ddra)
    }
    /// Level on the port B pins.
    pub fn port_b(&self) -> u8 {
        (self.prb & self.ddrb) | (self.pb_in & !self.ddrb)
    }

    /// True while this CIA is pulling its interrupt line active.
    pub fn irq_asserted(&self) -> bool {
        (self.icr_data & self.icr_mask & 0x1F) != 0
    }

    /// Advance both timers and the time-of-day clock by `cycles` phi2 clocks.
    pub fn tick(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.tick_one();
        }
        self.tick_tod(cycles);
    }

    fn tick_tod(&mut self, cycles: u32) {
        if !self.tod_running {
            return;
        }
        self.tod_acc += cycles;
        while self.tod_acc >= TOD_CYCLES_PER_TENTH {
            self.tod_acc -= TOD_CYCLES_PER_TENTH;
            self.tod_advance();
        }
    }

    fn tod_advance(&mut self) {
        // Tenths 0-9, then seconds and minutes in BCD (0-59), then the hour.
        if bcd_inc(&mut self.tod[0], 0x10)
            && bcd_inc(&mut self.tod[1], 0x60)
            && bcd_inc(&mut self.tod[2], 0x60)
        {
            self.tod_advance_hour();
        }
        // The alarm interrupt (ICR bit 2) latches when the time matches.
        if self.tod == self.tod_alarm {
            self.icr_data |= 0x04;
        }
    }

    fn tod_advance_hour(&mut self) {
        let mut pm = self.tod[3] & 0x80;
        let hr = self.tod[3] & 0x1F; // BCD 01..12
        let new_hr = match hr {
            0x11 => {
                pm ^= 0x80; // 11 -> 12 flips AM/PM
                0x12
            }
            0x12 => 0x01, // 12 -> 1
            _ => {
                let mut lo = (hr & 0x0F) + 1;
                let mut hi = hr >> 4;
                if lo > 9 {
                    lo = 0;
                    hi += 1;
                }
                (hi << 4) | lo
            }
        };
        self.tod[3] = pm | new_hr;
    }

    fn tick_one(&mut self) {
        // Timer A counts phi2 when started and in timed mode (CRA bit5 = 0).
        if self.cra & 0x01 != 0 && self.cra & 0x20 == 0 {
            let (t, under) = self.ta.overflowing_sub(1);
            self.ta = t;
            if under {
                self.icr_data |= INT_TA;
                self.ta = self.ta_latch; // reload
                if self.cra & 0x08 != 0 {
                    self.cra &= !0x01; // one-shot: stop
                }
            }
        }
        // Timer B counts phi2 when started and in timed mode (CRB bits 5-6 = 0).
        if self.crb & 0x01 != 0 && self.crb & 0x60 == 0 {
            let (t, under) = self.tb.overflowing_sub(1);
            self.tb = t;
            if under {
                self.icr_data |= INT_TB;
                self.tb = self.tb_latch;
                if self.crb & 0x08 != 0 {
                    self.crb &= !0x01;
                }
            }
        }
    }

    /// Read a register (offset 0x0..0xF). Reading ICR clears its flags.
    pub fn read(&mut self, reg: u8) -> u8 {
        match reg & 0x0F {
            PRA => self.port_a(),
            PRB => self.port_b(),
            DDRA => self.ddra,
            DDRB => self.ddrb,
            TA_LO => self.ta as u8,
            TA_HI => (self.ta >> 8) as u8,
            TB_LO => self.tb as u8,
            TB_HI => (self.tb >> 8) as u8,
            // Reading the hours *latches* all four registers; reading tenths
            // releases the latch. This keeps a program from catching the clock
            // mid-carry, and getting the order wrong is a classic way to hang.
            TOD_10TH => {
                let v = if self.tod_latched { self.tod_latch[0] } else { self.tod[0] };
                self.tod_latched = false;
                v
            }
            TOD_SEC => {
                if self.tod_latched {
                    self.tod_latch[1]
                } else {
                    self.tod[1]
                }
            }
            TOD_MIN => {
                if self.tod_latched {
                    self.tod_latch[2]
                } else {
                    self.tod[2]
                }
            }
            TOD_HR => {
                if !self.tod_latched {
                    self.tod_latch = self.tod;
                    self.tod_latched = true;
                }
                self.tod_latch[3]
            }
            SDR => self.sdr,
            ICR => {
                // Return pending flags + the "occurred" summary bit, then clear.
                let summary = if self.irq_asserted() { INT_FLAG } else { 0 };
                let v = (self.icr_data & 0x1F) | summary;
                self.icr_data = 0;
                v
            }
            CRA => self.cra,
            CRB => self.crb,
            _ => 0,
        }
    }

    /// Write a register (offset 0x0..0xF).
    pub fn write(&mut self, reg: u8, val: u8) {
        match reg & 0x0F {
            PRA => self.pra = val,
            PRB => self.prb = val,
            DDRA => self.ddra = val,
            DDRB => self.ddrb = val,
            TA_LO => self.ta_latch = (self.ta_latch & 0xFF00) | val as u16,
            TA_HI => self.ta_latch = (self.ta_latch & 0x00FF) | ((val as u16) << 8),
            TB_LO => self.tb_latch = (self.tb_latch & 0xFF00) | val as u16,
            TB_HI => self.tb_latch = (self.tb_latch & 0x00FF) | ((val as u16) << 8),
            // With CRB bit 7 set, TOD writes set the ALARM; otherwise the clock.
            // Writing the hour stops the clock; writing tenths starts it again,
            // so a program can set the whole time without it advancing mid-write.
            TOD_10TH => {
                if self.crb & 0x80 != 0 {
                    self.tod_alarm[0] = val & 0x0F;
                } else {
                    self.tod[0] = val & 0x0F;
                    self.tod_running = true;
                }
            }
            TOD_SEC => {
                if self.crb & 0x80 != 0 {
                    self.tod_alarm[1] = val & 0x7F;
                } else {
                    self.tod[1] = val & 0x7F;
                }
            }
            TOD_MIN => {
                if self.crb & 0x80 != 0 {
                    self.tod_alarm[2] = val & 0x7F;
                } else {
                    self.tod[2] = val & 0x7F;
                }
            }
            TOD_HR => {
                if self.crb & 0x80 != 0 {
                    self.tod_alarm[3] = val & 0x9F;
                } else {
                    self.tod[3] = val & 0x9F;
                    self.tod_running = false;
                }
            }
            SDR => self.sdr = val,
            ICR => {
                // Bit 7 = set/clear select for the mask bits below it.
                if val & 0x80 != 0 {
                    self.icr_mask |= val & 0x1F;
                } else {
                    self.icr_mask &= !(val & 0x1F);
                }
            }
            CRA => {
                if val & 0x10 != 0 {
                    self.ta = self.ta_latch; // force load (strobe)
                }
                self.cra = val & !0x10;
            }
            CRB => {
                if val & 0x10 != 0 {
                    self.tb = self.tb_latch;
                }
                self.crb = val & !0x10;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddr_masks_port_reads() {
        let mut cia = Cia::new();
        cia.pa_in = 0b1111_0000;
        cia.write(DDRA, 0x0F); // low nibble output
        cia.write(PRA, 0x0A);
        assert_eq!(cia.read(PRA), 0b1111_1010);
    }

    #[test]
    fn tod_advances_ten_times_a_second() {
        let mut cia = Cia::new();
        // Start at 00:00:00.0 so the maths is easy.
        cia.write(TOD_HR, 0x00);
        cia.write(TOD_MIN, 0x00);
        cia.write(TOD_SEC, 0x00);
        cia.write(TOD_10TH, 0x00); // writing tenths starts the clock
        cia.tick(TOD_CYCLES_PER_TENTH * 3);
        assert_eq!(cia.read(TOD_10TH), 0x03, "three tenths elapsed");
        // A full second rolls tenths back to 0 and bumps seconds (BCD).
        cia.tick(TOD_CYCLES_PER_TENTH * 7);
        cia.read(TOD_HR); // latch, then read down to release
        assert_eq!(cia.read(TOD_MIN), 0x00);
        assert_eq!(cia.read(TOD_SEC), 0x01);
        assert_eq!(cia.read(TOD_10TH), 0x00);
    }

    #[test]
    fn seconds_carry_in_bcd() {
        let mut cia = Cia::new();
        cia.write(TOD_HR, 0x00);
        cia.write(TOD_MIN, 0x00);
        cia.write(TOD_SEC, 0x59); // 59 seconds
        cia.write(TOD_10TH, 0x09); // .9, clock running
        cia.tick(TOD_CYCLES_PER_TENTH); // -> 1 minute, 0 seconds
        cia.read(TOD_HR);
        assert_eq!(cia.read(TOD_MIN), 0x01);
        assert_eq!(cia.read(TOD_SEC), 0x00);
    }

    #[test]
    fn reading_hours_latches_until_tenths_are_read() {
        let mut cia = Cia::new();
        cia.write(TOD_HR, 0x00);
        cia.write(TOD_SEC, 0x00);
        cia.write(TOD_10TH, 0x00);
        cia.read(TOD_HR); // latch the current time
        cia.tick(TOD_CYCLES_PER_TENTH * 25); // clock keeps running underneath
        assert_eq!(cia.read(TOD_SEC), 0x00, "latched read is frozen");
        assert_eq!(cia.read(TOD_10TH), 0x00, "reading tenths releases the latch");
        // Now a fresh read sees the advanced time.
        cia.read(TOD_HR);
        assert_eq!(cia.read(TOD_SEC), 0x02); // 25 tenths = 2.5s
    }

    #[test]
    fn timer_a_fires_and_reloads_like_the_jiffy_irq() {
        let mut cia = Cia::new();
        cia.write(ICR, 0x80 | INT_TA); // enable Timer A interrupt
        cia.write(TA_LO, 0x03);
        cia.write(TA_HI, 0x00);
        cia.write(CRA, 0x11); // force-load + start, continuous
        assert!(!cia.irq_asserted());
        cia.tick(3); // 3 -> 2 -> 1 -> 0
        assert!(!cia.irq_asserted());
        cia.tick(1); // underflow
        assert!(cia.irq_asserted());
        // Reading ICR reports and clears the flag.
        let icr = cia.read(ICR);
        assert_eq!(icr & INT_TA, INT_TA);
        assert_eq!(icr & INT_FLAG, INT_FLAG);
        assert!(!cia.irq_asserted());
        // Continuous mode reloaded from the latch and keeps running.
        cia.tick(4);
        assert!(cia.irq_asserted());
    }

    #[test]
    fn masked_interrupt_does_not_assert() {
        let mut cia = Cia::new();
        // Do NOT enable the interrupt in the mask.
        cia.write(TA_LO, 0x01);
        cia.write(TA_HI, 0x00);
        cia.write(CRA, 0x11);
        cia.tick(2);
        assert!(!cia.irq_asserted()); // flag set internally, but masked off
        assert_eq!(cia.read(ICR) & INT_TA, INT_TA); // flag is still visible
    }
}
