//! The Commodore serial bus — "IEC" — that connects the C64 to its disk drives.
//!
//! Electrically this is the simplest interesting thing in the whole machine:
//! three signal wires, each an **open-collector** line with a pull-up resistor.
//! No device ever drives a line high. A device either *pulls it low* or *lets
//! go*, and the line sits high only when **every** device has let go. That makes
//! the bus a wired-AND of "released" (equivalently, a wired-OR of "pulling").
//!
//! ```text
//!            +5V
//!             │
//!            [R]  pull-up
//!             │
//!   ──────────┼───────────┬───────────┬──────────  one line (e.g. CLK)
//!             │           │           │
//!          ┌──┴──┐     ┌──┴──┐     ┌──┴──┐
//!          │ C64 │     │ 8   │     │ 9   │         each can only pull down
//!          └─────┘     └─────┘     └─────┘
//! ```
//!
//! The three lines:
//!
//! | Line  | Driven by | Meaning |
//! |-------|-----------|---------|
//! | `ATN`  | controller only | "attention": what follows is a command, not data |
//! | `CLK`  | whoever is talking | clocks each bit; also the handshake heartbeat |
//! | `DATA` | talker (bits) / listeners (acknowledge) | the bit value, and "I'm ready" / "got it" |
//!
//! (A fourth wire, `SRQ`, is unused on the C64 — the VIC-20 heritage left it
//! disconnected — and a fifth is just ground. Neither is modelled.)
//!
//! ## A note on polarity, because it is the number one source of bugs
//!
//! Commodore's documentation calls a line "true" when it is **pulled low**, and
//! the chips on both ends invert: writing a `1` to CIA2's ATN bit *pulls ATN
//! low*. Two inversions in a row is how you end up debugging for a week, so this
//! crate refuses to play along. Here a line is described **only** by its
//! electrical level:
//!
//! - [`Bus::released`] — nobody is pulling; the line is **high**.
//! - [`Bus::pulled_low`] — at least one device is pulling; the line is **low**.
//!
//! The chip-register inversions live at the edges, in the code that wires a CIA
//! or a VIA to this bus, where they can be documented against the datasheet.
//!
//! `no_std` and allocation-free: the bus is three bits of state per device.

#![no_std]

pub mod device;

pub use device::{Device, Storage, DEFAULT_ADDRESS};

/// Bit masks used to talk about a set of lines at once.
pub mod line {
    /// ATN — "attention". Only the controller (the C64) drives this.
    pub const ATN: u8 = 1 << 0;
    /// CLK — the clock/handshake line.
    pub const CLK: u8 = 1 << 1;
    /// DATA — the data/acknowledge line.
    pub const DATA: u8 = 1 << 2;
    /// All three lines.
    pub const ALL: u8 = ATN | CLK | DATA;
}

/// One of the three bus lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Atn,
    Clk,
    Data,
}

impl Line {
    /// The [`line`] bit mask for this line.
    pub const fn mask(self) -> u8 {
        match self {
            Line::Atn => line::ATN,
            Line::Clk => line::CLK,
            Line::Data => line::DATA,
        }
    }
}

/// How many devices can share the bus. Slot 0 is the controller; a real C64
/// serial chain tops out at four or five peripherals before the pull-ups give
/// up, so this is generous.
pub const MAX_DEVICES: usize = 4;

/// The controller's slot. On a C64 this is always the computer itself — it is
/// the only device allowed to pull ATN.
pub const CONTROLLER: usize = 0;

/// The three shared lines, plus who is pulling each one.
///
/// Devices don't own lines; they own an opinion about each line, and the bus
/// resolves those opinions. Every device gets a slot and declares which lines it
/// is currently pulling low ([`Bus::set_pulls`] / [`Bus::pull`]); everyone then
/// reads the same resolved levels ([`Bus::released`] / [`Bus::pulled_low`]).
#[derive(Debug, Clone)]
pub struct Bus {
    /// `pulls[d]` is the set of lines device `d` is pulling low.
    pulls: [u8; MAX_DEVICES],
}

impl Default for Bus {
    fn default() -> Self {
        Bus {
            // Power-on: nobody pulls anything, so all three lines float high.
            pulls: [0; MAX_DEVICES],
        }
    }
}

impl Bus {
    /// A fresh, idle bus: all three lines released (high).
    pub fn new() -> Self {
        Self::default()
    }

    /// The set of lines *somebody* is pulling low.
    pub fn pulled_mask(&self) -> u8 {
        let mut m = 0;
        for p in self.pulls {
            m |= p;
        }
        m
    }

    /// Is `line` released — i.e. **high**, because no device is pulling it?
    pub fn released(&self, line: Line) -> bool {
        self.pulled_mask() & line.mask() == 0
    }

    /// Is `line` pulled **low** by at least one device?
    pub fn pulled_low(&self, line: Line) -> bool {
        !self.released(line)
    }

    /// ATN level: `true` = high (released), `false` = low.
    pub fn atn(&self) -> bool {
        self.released(Line::Atn)
    }
    /// CLK level: `true` = high (released), `false` = low.
    pub fn clk(&self) -> bool {
        self.released(Line::Clk)
    }
    /// DATA level: `true` = high (released), `false` = low.
    pub fn data(&self) -> bool {
        self.released(Line::Data)
    }

    /// Replace everything device `dev` is pulling with `mask` (a set of [`line`]
    /// bits). This is what a chip-to-bus adapter calls after its port register
    /// changes: it recomputes the whole opinion in one go.
    ///
    /// Out-of-range slots are ignored rather than panicking — this runs inside a
    /// memory-mapped register write, which is no place for a panic.
    pub fn set_pulls(&mut self, dev: usize, mask: u8) {
        if dev < MAX_DEVICES {
            self.pulls[dev] = mask & line::ALL;
        }
    }

    /// Start (`true`) or stop (`false`) pulling a single line low, leaving this
    /// device's other lines alone.
    pub fn pull(&mut self, dev: usize, line: Line, pulling: bool) {
        if dev >= MAX_DEVICES {
            return;
        }
        if pulling {
            self.pulls[dev] |= line.mask();
        } else {
            self.pulls[dev] &= !line.mask();
        }
    }

    /// What device `dev` is currently pulling.
    pub fn pulls_of(&self, dev: usize) -> u8 {
        if dev < MAX_DEVICES {
            self.pulls[dev]
        } else {
            0
        }
    }

    /// Let go of every line this device holds — a device reset, or one dropping
    /// off the chain.
    pub fn release_all(&mut self, dev: usize) {
        self.set_pulls(dev, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_bus_floats_high() {
        let bus = Bus::new();
        assert!(bus.atn() && bus.clk() && bus.data());
        assert!(bus.released(Line::Clk));
        assert!(!bus.pulled_low(Line::Clk));
    }

    /// The defining property: one device pulling is enough to take the line low,
    /// and it stays low until *everyone* lets go.
    #[test]
    fn any_device_pulling_takes_the_line_low() {
        let mut bus = Bus::new();
        bus.pull(CONTROLLER, Line::Data, true);
        bus.pull(2, Line::Data, true);
        assert!(bus.pulled_low(Line::Data));

        // The controller lets go — device 2 is still holding it down.
        bus.pull(CONTROLLER, Line::Data, false);
        assert!(bus.pulled_low(Line::Data), "device 2 still pulls DATA low");

        // Now everyone has let go.
        bus.pull(2, Line::Data, false);
        assert!(bus.released(Line::Data));
    }

    #[test]
    fn lines_are_independent() {
        let mut bus = Bus::new();
        bus.pull(CONTROLLER, Line::Atn, true);
        assert!(bus.pulled_low(Line::Atn));
        assert!(bus.clk() && bus.data(), "ATN must not disturb CLK or DATA");
    }

    #[test]
    fn set_pulls_replaces_the_whole_opinion() {
        let mut bus = Bus::new();
        bus.set_pulls(1, line::CLK | line::DATA);
        assert!(bus.pulled_low(Line::Clk) && bus.pulled_low(Line::Data));

        // Writing a new mask drops CLK and keeps DATA — no leftover state.
        bus.set_pulls(1, line::DATA);
        assert!(bus.released(Line::Clk));
        assert!(bus.pulled_low(Line::Data));
        assert_eq!(bus.pulls_of(1), line::DATA);
    }

    #[test]
    fn release_all_drops_only_that_device() {
        let mut bus = Bus::new();
        bus.set_pulls(CONTROLLER, line::ALL);
        bus.set_pulls(1, line::CLK);
        bus.release_all(CONTROLLER);
        assert!(bus.atn() && bus.data(), "controller let go of ATN and DATA");
        assert!(bus.pulled_low(Line::Clk), "device 1 still holds CLK");
    }

    /// A memory-mapped register write must never panic, however wrong the slot.
    #[test]
    fn out_of_range_slots_are_ignored() {
        let mut bus = Bus::new();
        bus.set_pulls(MAX_DEVICES, line::ALL);
        bus.pull(99, Line::Atn, true);
        assert_eq!(bus.pulled_mask(), 0);
        assert_eq!(bus.pulls_of(99), 0);
    }
}
