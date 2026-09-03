//! MOS 6522 VIA (Versatile Interface Adapter).
//!
//! The 1541 drive uses two of these: VIA1 ($1800) drives the serial IEC bus,
//! VIA2 ($1C00) drives the disk controller (motor, stepper, read/write head).
//!
//! This models the parts the drive firmware actually depends on to boot and run:
//! the two I/O ports with data-direction registers, both interval timers (T1 and
//! T2) with their interrupt flags, and the IFR/IER interrupt logic. The shift
//! register and the fine details of CA/CB handshaking are stubbed — the drive's
//! boot path and idle loop don't need them, and they can be filled in when we
//! emulate real disk I/O.
//!
//! `no_std`, MCU-ready.

#![no_std]

// Register offsets (0x0..0xF), as seen at the chip's 16-byte window.
const ORB: u8 = 0x0; // port B data
const ORA: u8 = 0x1; // port A data
const DDRB: u8 = 0x2;
const DDRA: u8 = 0x3;
const T1CL: u8 = 0x4; // timer 1 counter low
const T1CH: u8 = 0x5; // timer 1 counter high
const T1LL: u8 = 0x6; // timer 1 latch low
const T1LH: u8 = 0x7; // timer 1 latch high
const T2CL: u8 = 0x8; // timer 2 counter low
const T2CH: u8 = 0x9; // timer 2 counter high
const SR: u8 = 0xA; // shift register (stubbed)
const ACR: u8 = 0xB; // auxiliary control register
const PCR: u8 = 0xC; // peripheral control register
const IFR: u8 = 0xD; // interrupt flag register
const IER: u8 = 0xE; // interrupt enable register
const ORA_NH: u8 = 0xF; // port A, no handshake

// IFR / IER bit positions.
const FLAG_T2: u8 = 0x20;
const FLAG_T1: u8 = 0x40;
const FLAG_IRQ: u8 = 0x80;

/// A single 6522 VIA.
#[derive(Debug, Clone)]
pub struct Via {
    // Ports
    orb: u8,
    ora: u8,
    ddrb: u8,
    ddra: u8,
    /// Levels present on the port A / port B input pins (open-bus defaults high).
    pub pa_in: u8,
    pub pb_in: u8,

    // Timer 1
    t1_counter: u16,
    t1_latch: u16,
    t1_pb7: bool, // PB7 output level in T1 PB7 mode

    // Timer 2
    t2_counter: u16,
    t2_latch_lo: u8,
    t2_armed: bool, // one-shot fires once until reloaded

    // Control / interrupts
    acr: u8,
    pcr: u8,
    ifr: u8, // without the summary bit 7
    ier: u8, // without bit 7
    sr: u8,
}

impl Default for Via {
    fn default() -> Self {
        Via {
            orb: 0,
            ora: 0,
            ddrb: 0,
            ddra: 0,
            pa_in: 0xFF,
            pb_in: 0xFF,
            t1_counter: 0,
            t1_latch: 0,
            t1_pb7: false,
            t2_counter: 0,
            t2_latch_lo: 0,
            t2_armed: false,
            acr: 0,
            pcr: 0,
            ifr: 0,
            ier: 0,
            sr: 0,
        }
    }
}

impl Via {
    pub fn new() -> Self {
        Self::default()
    }

    /// Level on the port B pins: output bits come from ORB (where DDRB=1),
    /// input bits from the input pins (where DDRB=0).
    pub fn port_b(&self) -> u8 {
        (self.orb & self.ddrb) | (self.pb_in & !self.ddrb)
    }

    /// Level on the port A pins.
    pub fn port_a(&self) -> u8 {
        (self.ora & self.ddra) | (self.pa_in & !self.ddra)
    }

    /// True while this VIA is pulling the shared IRQ line low.
    pub fn irq_asserted(&self) -> bool {
        (self.ifr & self.ier & 0x7F) != 0
    }

    /// Advance both timers by `cycles` system clocks.
    pub fn tick(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.tick_one();
        }
    }

    fn tick_one(&mut self) {
        // Timer 1: always counts. Underflow sets the T1 flag; in free-run mode
        // (ACR bit 6) it reloads from the latch and, if enabled, toggles PB7.
        let (t1, under1) = self.t1_counter.overflowing_sub(1);
        self.t1_counter = t1;
        if under1 {
            self.ifr |= FLAG_T1;
            if self.acr & 0x40 != 0 {
                self.t1_counter = self.t1_latch;
                if self.acr & 0x80 != 0 {
                    self.t1_pb7 = !self.t1_pb7;
                }
            }
        }

        // Timer 2: in timed one-shot mode (ACR bit 5 = 0) it counts down and
        // fires once on underflow. Pulse-counting mode is not modeled.
        if self.acr & 0x20 == 0 {
            let (t2, under2) = self.t2_counter.overflowing_sub(1);
            self.t2_counter = t2;
            if under2 && self.t2_armed {
                self.ifr |= FLAG_T2;
                self.t2_armed = false;
            }
        }
    }

    /// Read a register (offset 0x0..0xF). Some reads clear interrupt flags.
    pub fn read(&mut self, reg: u8) -> u8 {
        match reg & 0x0F {
            ORB => self.port_b(),
            ORA | ORA_NH => self.port_a(),
            DDRB => self.ddrb,
            DDRA => self.ddra,
            T1CL => {
                self.ifr &= !FLAG_T1; // reading T1 low clears its flag
                self.t1_counter as u8
            }
            T1CH => (self.t1_counter >> 8) as u8,
            T1LL => self.t1_latch as u8,
            T1LH => (self.t1_latch >> 8) as u8,
            T2CL => {
                self.ifr &= !FLAG_T2; // reading T2 low clears its flag
                self.t2_counter as u8
            }
            T2CH => (self.t2_counter >> 8) as u8,
            SR => self.sr,
            ACR => self.acr,
            PCR => self.pcr,
            IFR => {
                // Bit 7 summarizes "any enabled interrupt pending".
                let summary = if self.irq_asserted() { FLAG_IRQ } else { 0 };
                (self.ifr & 0x7F) | summary
            }
            IER => self.ier | 0x80, // bit 7 always reads 1
            _ => 0,
        }
    }

    /// Write a register (offset 0x0..0xF).
    pub fn write(&mut self, reg: u8, val: u8) {
        match reg & 0x0F {
            ORB => self.orb = val,
            ORA | ORA_NH => self.ora = val,
            DDRB => self.ddrb = val,
            DDRA => self.ddra = val,
            T1CL | T1LL => self.t1_latch = (self.t1_latch & 0xFF00) | val as u16,
            T1CH => {
                self.t1_latch = (self.t1_latch & 0x00FF) | ((val as u16) << 8);
                self.t1_counter = self.t1_latch; // load and start
                self.ifr &= !FLAG_T1;
            }
            T1LH => {
                self.t1_latch = (self.t1_latch & 0x00FF) | ((val as u16) << 8);
                self.ifr &= !FLAG_T1;
            }
            T2CL => self.t2_latch_lo = val,
            T2CH => {
                self.t2_counter = ((val as u16) << 8) | self.t2_latch_lo as u16;
                self.ifr &= !FLAG_T2;
                self.t2_armed = true; // one-shot re-armed
            }
            SR => self.sr = val,
            ACR => self.acr = val,
            PCR => self.pcr = val,
            IFR => self.ifr &= !(val & 0x7F), // writing 1 clears that flag
            IER => {
                if val & 0x80 != 0 {
                    self.ier |= val & 0x7F; // set selected enables
                } else {
                    self.ier &= !(val & 0x7F); // clear selected enables
                }
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
        let mut via = Via::new();
        via.pb_in = 0b1010_1010;
        via.write(DDRB, 0b0000_1111); // low nibble output, high nibble input
        via.write(ORB, 0b0000_0101); // drive low nibble
        // Low nibble from ORB (0101), high nibble from input pins (1010).
        assert_eq!(via.read(ORB), 0b1010_0101);
    }

    #[test]
    fn timer1_one_shot_sets_flag_on_underflow() {
        let mut via = Via::new();
        via.write(IER, 0x80 | FLAG_T1); // enable T1 interrupt
        via.write(T1CL, 0x03);
        via.write(T1CH, 0x00); // load counter = 3, start
        assert!(!via.irq_asserted());
        via.tick(3); // 3 -> 2 -> 1 -> 0
        assert!(!via.irq_asserted());
        via.tick(1); // 0 -> underflow
        assert!(via.irq_asserted());
        assert_eq!(via.read(IFR) & FLAG_T1, FLAG_T1);
        // Reading T1 low clears the flag.
        via.read(T1CL);
        assert!(!via.irq_asserted());
    }

    #[test]
    fn timer1_free_run_reloads() {
        let mut via = Via::new();
        via.write(ACR, 0x40); // free-run
        via.write(IER, 0x80 | FLAG_T1);
        via.write(T1CL, 0x01);
        via.write(T1CH, 0x00); // counter = 1
        via.tick(2); // underflow, reloads to latch (1)
        assert!(via.irq_asserted());
        via.write(IFR, FLAG_T1); // clear
        assert!(!via.irq_asserted());
        via.tick(2); // fires again after reload
        assert!(via.irq_asserted());
    }

    #[test]
    fn ier_set_and_clear() {
        let mut via = Via::new();
        via.write(IER, 0x80 | FLAG_T1 | FLAG_T2); // enable both
        assert_eq!(via.read(IER), 0x80 | FLAG_T1 | FLAG_T2);
        via.write(IER, FLAG_T1); // bit7=0 -> clear T1 enable
        assert_eq!(via.read(IER), 0x80 | FLAG_T2);
    }
}
