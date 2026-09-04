//! A peripheral on the serial bus, as a state machine.
//!
//! This is the other half of the conversation the KERNAL's `$ED40` (send) and
//! `$EE13` (receive) routines have. It is not a 1541 — there is no drive CPU, no
//! DOS, no read head here. It is the *protocol*: something that answers to a
//! device number, understands LISTEN/TALK/OPEN/CLOSE, and clocks bytes in and
//! out one line-transition at a time. Where the bytes come from is somebody
//! else's problem, expressed as the [`Storage`] trait.
//!
//! Being able to plug this in matters for two reasons: it makes the C64's own
//! serial code testable end-to-end against the real ROM, and it isolates the
//! protocol from the drive so that when the real 1541 firmware is wired to the
//! same [`Bus`] there is a known-good reference to compare against.
//!
//! # The protocol, as read off the KERNAL
//!
//! Every line reference below is to the disassembly in `docs/kernal-boot.md`'s
//! companion listing. Three rules carry almost all of it:
//!
//! 1. **`DATA` high means a one.** `$ED73`: the KERNAL rotates the byte and
//!    branches to DATAHI (release) on a set bit, DATALO (pull) on a clear one.
//! 2. **A bit is read on the `CLK` rising edge.** The talker puts the bit on
//!    `DATA`, *then* releases `CLK` (`$ED7D`); the listener spins until `CLK`
//!    goes high and samples `DATA` right there (`$EE62`-`$EE65`).
//! 3. **A listener holds `DATA` low when it is not ready.** That single
//!    convention does three jobs: it acknowledges attention, it acknowledges
//!    each received byte (`$EDA6`), and — because a bus with nobody on it has
//!    `DATA` floating high — it *is* the "device present" signal (`$ED47`
//!    reports `DEVICE NOT PRESENT` when `DATA` reads high).
//!
//! The one genuinely odd corner is **EOI**, the way a talker says "this is the
//! last byte". There is no flag bit and no extra line. The talker simply
//! *dawdles*: instead of pulling `CLK` low to start the byte, it waits. The
//! listener is timing that gap with CIA1 Timer B (`$EE20`, ~256 µs) and treats
//! the timeout as end-of-file, answering with a brief `DATA`-low pulse
//! (`$EE47`) before receiving the final byte normally. Silence, timed.

use crate::{Bus, Line};

/// Where a device's bytes come from.
///
/// Deliberately tiny and byte-at-a-time: it keeps this crate `no_std` and
/// allocation-free, and it means a source can be a disk image, a directory
/// listing synthesised on the fly, or a test fixture.
pub trait Storage {
    /// Open `name` on `channel` (the secondary address). `false` means "no such
    /// file" — the device then has nothing to send.
    fn open(&mut self, channel: u8, name: &[u8]) -> bool;

    /// The next byte of `channel`, or `None` at end of file.
    fn read(&mut self, channel: u8) -> Option<u8>;

    /// A byte sent *to* the device on `channel` (a SAVE). The default drops it.
    fn write(&mut self, channel: u8, byte: u8) {
        let _ = (channel, byte);
    }

    /// Close `channel`.
    fn close(&mut self, channel: u8) {
        let _ = channel;
    }
}

/// The device number a 1541 answers to out of the box.
pub const DEFAULT_ADDRESS: u8 = 8;

/// The longest filename we will buffer (a CBM name is 16 characters, but a
/// LOAD can carry options and `$` patterns, so leave room).
const NAME_MAX: usize = 40;

// ---------------------------------------------------------------------------
// Timing.
//
// One PAL C64 cycle is 1.0150 µs, near enough to call these numbers
// microseconds. Everything here is a *floor*, not a faithful reproduction of
// 1541 timing: the requirement is only that each pulse outlast the KERNAL's
// polling loop and that the EOI stall outlast the KERNAL's timer.
// ---------------------------------------------------------------------------

/// How long to hold a level before changing another line. The KERNAL polls the
/// bus with `LDA $DD00; CMP $DD00; BNE; ASL` — about 13 cycles — so anything
/// this side of ~15 is at risk of being blinked through. 20 is comfortable.
const SETTLE: u32 = 20;

/// How long `CLK` stays high with a valid bit on `DATA`. Same reasoning as
/// [`SETTLE`], with more margin because missing a bit desynchronises the byte.
const BIT_HOLD: u32 = 40;

/// The EOI stall: how long a talker sits on its hands to mean "last byte".
/// Must comfortably exceed the listener's timeout — CIA1 Timer B is loaded with
/// `$01xx` at `$EE20`, so ~256-511 µs.
const EOI_STALL: u32 = 700;

/// How long a listener holds `DATA` low to acknowledge an EOI it detected.
/// `$EE47`-`$EE52` does DATALO, CLKHI, set status, and loops back — brief.
const EOI_ACK_HOLD: u32 = 60;

/// A listener that has not acknowledged a byte in this long has gone away.
const ACK_TIMEOUT: u32 = 4000;

/// How long a freshly turned-around talker holds `CLK` low before it is ready
/// to send.
///
/// This one is load-bearing, and getting it wrong deadlocks the bus. After
/// TKSA the KERNAL releases ATN and then sits at `$EDD6` (`JSR $EEA9; BMI
/// $EDD6`) waiting to see `CLK` pulled **low** — that is how it knows the
/// device has taken over as talker. Release `CLK` again before the KERNAL gets
/// there and it waits forever, while the device waits for the `DATA` release
/// that only comes *after* that loop. Both sides hang, politely.
///
/// A real 1541 is in no danger of this: it has to go and fetch the byte first,
/// which takes milliseconds. Hold long enough to be unmissable.
const TURNAROUND_HOLD: u32 = 400;

/// How long a *listener* waits for `CLK` to go low before calling it EOI. The
/// mirror of [`EOI_STALL`], and the same role CIA1 Timer B plays for the C64.
const RX_EOI_TIMEOUT: u32 = 400;

/// What the controller has told this device to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Not addressed: keep off the bus entirely.
    Unaddressed,
    /// Addressed to LISTEN — bytes are coming to us.
    Listener,
    /// Addressed to TALK — we send, once ATN is released.
    Talker,
}

/// Where we are in the current line-level handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Off the bus, nothing to do.
    Idle,

    // -- receiving ---------------------------------------------------------
    /// Attention acknowledged, `DATA` held low, waiting for the controller to
    /// pull `CLK` low. Only after that does a released `CLK` mean "ready to
    /// send" — without this step an idle high `CLK` would be misread as the
    /// go-ahead and we would let `DATA` go far too early.
    RxWaitClkStart,
    /// Holding `DATA` low ("here, and not ready"). When the talker releases
    /// `CLK` it is ready to send, and we answer by releasing `DATA`.
    RxWaitClkHigh,
    /// `DATA` released. Waiting for `CLK` to go low, which starts the byte —
    /// or for [`RX_EOI_TIMEOUT`] to pass, which means EOI.
    RxWaitClkLow,
    /// Pulsing `DATA` low to acknowledge an EOI.
    RxEoiAck,
    /// Waiting for the `CLK` rising edge that makes the next bit valid.
    RxBitWaitHigh,
    /// Bit taken; waiting for `CLK` to fall again.
    RxBitWaitLow,

    // -- sending -----------------------------------------------------------
    /// Just turned around: claim the bus as talker by pulling `CLK` low.
    TxTurnaround,
    /// Release `CLK` ("ready to send") and wait for the listener to release
    /// `DATA` ("ready for data").
    TxReady,
    /// The EOI stall — say "last byte" by doing nothing, conspicuously.
    TxEoiStall,
    /// The listener pulsed `DATA` low to acknowledge EOI; wait for it to let go.
    TxEoiAckRelease,
    /// Pull `CLK` low to open the byte.
    TxStartByte,
    /// Put the current bit on `DATA`, then release `CLK`.
    TxBitSet,
    /// `CLK` high with the bit valid; hold, then close the bit.
    TxBitHold,
    /// All eight bits gone; wait for the listener to pull `DATA` low.
    TxWaitAck,
    /// Nothing left to send. `CLK` released so a listener's wait ends in a
    /// clean timeout rather than a hang.
    TxDone,
}

/// A virtual peripheral on the serial bus.
///
/// Clock it once per CPU cycle with [`Device::tick`], alongside whatever else
/// the machine clocks. It only ever touches its own slot on the [`Bus`], so the
/// wired-OR resolution does the arbitration for free.
pub struct Device {
    address: u8,
    slot: usize,
    role: Role,
    state: State,
    /// Cycles spent in the current state.
    timer: u32,

    // Edge detection.
    prev_atn: bool,
    /// True while ATN is asserted, so a received byte is a command.
    under_atn: bool,

    // Byte in flight.
    bit: u8,
    shift: u8,
    /// Set when we detected EOI on an incoming byte.
    rx_eoi: bool,
    /// Guard so one stall is only ever read as one EOI.
    rx_eoi_done: bool,

    /// Secondary address (channel) of the current command.
    channel: u8,
    /// Channel an OPEN is collecting a name for.
    opening: Option<u8>,
    name: [u8; NAME_MAX],
    name_len: usize,

    /// One-byte lookahead, so we know which byte is the last one and can flag
    /// EOI *before* sending it.
    lookahead: Option<u8>,
    tx_eoi: bool,
}

impl Device {
    /// A device answering to `address` (8 for the first drive), occupying `slot`
    /// on the bus. Use any slot but [`crate::CONTROLLER`].
    pub fn new(address: u8, slot: usize) -> Self {
        Device {
            address,
            slot,
            role: Role::Unaddressed,
            state: State::Idle,
            timer: 0,
            prev_atn: true,
            under_atn: false,
            bit: 0,
            shift: 0,
            rx_eoi: false,
            rx_eoi_done: false,
            channel: 0,
            opening: None,
            name: [0; NAME_MAX],
            name_len: 0,
            lookahead: None,
            tx_eoi: false,
        }
    }

    /// The device number this answers to.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// True while the controller has this device addressed to talk or listen.
    pub fn is_addressed(&self) -> bool {
        self.role != Role::Unaddressed
    }

    fn pull(&self, bus: &mut Bus, line: Line, pulling: bool) {
        bus.pull(self.slot, line, pulling);
    }

    /// Advance by `cycles`, in single-cycle steps.
    ///
    /// The handshake is all edges, and an edge that is set and cleared inside
    /// one batch is an edge that never happened — so this never coarsens the
    /// step. It is the same choice [`crate::Bus`]'s users make for the CIA.
    pub fn tick<S: Storage>(&mut self, bus: &mut Bus, storage: &mut S, cycles: u32) {
        for _ in 0..cycles {
            self.tick_one(bus, storage);
        }
    }

    fn tick_one<S: Storage>(&mut self, bus: &mut Bus, storage: &mut S) {
        let atn = bus.atn();
        let clk = bus.clk();
        let data = bus.data();

        // ATN is the controller interrupting whatever was going on. On a real
        // 1541 this is wired to VIA1's CA1 and raises an interrupt; the DATA
        // acknowledge is done in hardware, which is why it looks instant here.
        if self.prev_atn && !atn {
            self.attention(bus);
        } else if !self.prev_atn && atn {
            self.attention_released(bus, storage);
        }
        self.prev_atn = atn;

        self.timer = self.timer.saturating_add(1);

        match self.state {
            State::Idle | State::TxDone => {}

            // ---- receiving -------------------------------------------------
            State::RxWaitClkStart => {
                if !clk {
                    self.enter(State::RxWaitClkHigh);
                }
            }
            State::RxWaitClkHigh => {
                if clk {
                    // Talker is ready. Say we are too, by letting DATA go.
                    self.pull(bus, Line::Data, false);
                    self.enter(State::RxWaitClkLow);
                    self.rx_eoi_done = false;
                }
            }
            State::RxWaitClkLow => {
                if !clk {
                    self.bit = 0;
                    self.shift = 0;
                    self.enter(State::RxBitWaitHigh);
                } else if self.timer > RX_EOI_TIMEOUT && !self.rx_eoi_done {
                    // The talker is stalling: that is EOI. Acknowledge it.
                    self.rx_eoi = true;
                    self.rx_eoi_done = true;
                    self.pull(bus, Line::Data, true);
                    self.enter(State::RxEoiAck);
                }
            }
            State::RxEoiAck => {
                if self.timer >= EOI_ACK_HOLD {
                    self.pull(bus, Line::Data, false);
                    self.enter(State::RxWaitClkLow);
                }
            }
            State::RxBitWaitHigh => {
                if clk {
                    // Rising edge: DATA is valid *now*. High is a one, and the
                    // byte arrives least-significant bit first.
                    self.shift >>= 1;
                    if data {
                        self.shift |= 0x80;
                    }
                    self.bit += 1;
                    self.enter(State::RxBitWaitLow);
                }
            }
            State::RxBitWaitLow => {
                if !clk {
                    if self.bit >= 8 {
                        // Acknowledge the frame, then act on it.
                        self.pull(bus, Line::Data, true);
                        let byte = self.shift;
                        self.enter(State::RxWaitClkHigh);
                        self.byte_received(bus, storage, byte);
                    } else {
                        self.enter(State::RxBitWaitHigh);
                    }
                }
            }

            // ---- sending ---------------------------------------------------
            State::TxTurnaround => {
                self.pull(bus, Line::Clk, true);
                self.pull(bus, Line::Data, false);
                if self.timer >= TURNAROUND_HOLD {
                    self.enter(State::TxReady);
                }
            }
            State::TxReady => {
                self.pull(bus, Line::Clk, false); // "ready to send"
                if data {
                    // Listener has released DATA: it is ready for a byte.
                    match self.lookahead.take() {
                        None => self.enter(State::TxDone),
                        Some(byte) => {
                            self.shift = byte;
                            self.lookahead = storage.read(self.channel);
                            self.tx_eoi = self.lookahead.is_none();
                            self.bit = 0;
                            self.enter(if self.tx_eoi {
                                State::TxEoiStall
                            } else {
                                State::TxStartByte
                            });
                        }
                    }
                }
            }
            State::TxEoiStall => {
                // Saying "last byte" by not saying anything for long enough.
                if !data {
                    // The listener noticed and is pulsing DATA low.
                    self.enter(State::TxEoiAckRelease);
                } else if self.timer > EOI_STALL + ACK_TIMEOUT {
                    // Listener never acknowledged; send it anyway.
                    self.enter(State::TxStartByte);
                }
            }
            State::TxEoiAckRelease => {
                if data {
                    self.enter(State::TxStartByte);
                }
            }
            State::TxStartByte => {
                self.pull(bus, Line::Clk, true);
                if self.timer >= SETTLE {
                    self.enter(State::TxBitSet);
                }
            }
            State::TxBitSet => {
                // Bit on DATA first (high = 1), CLK released after — the
                // listener is watching for that rising edge.
                let one = self.shift & 0x01 != 0;
                self.pull(bus, Line::Data, !one);
                if self.timer >= SETTLE {
                    self.pull(bus, Line::Clk, false);
                    self.enter(State::TxBitHold);
                }
            }
            State::TxBitHold => {
                if self.timer >= BIT_HOLD {
                    // Close the bit: CLK low again, and let DATA go so the
                    // listener can use it to acknowledge.
                    self.pull(bus, Line::Clk, true);
                    self.pull(bus, Line::Data, false);
                    self.shift >>= 1;
                    self.bit += 1;
                    self.enter(if self.bit >= 8 {
                        State::TxWaitAck
                    } else {
                        State::TxBitSet
                    });
                }
            }
            State::TxWaitAck => {
                if !data {
                    self.enter(State::TxReady); // acknowledged; next byte
                } else if self.timer > ACK_TIMEOUT {
                    self.enter(State::TxDone);
                }
            }
        }

        if self.state == State::TxDone {
            // Leave CLK released: a listener still waiting then ends in the
            // KERNAL's clean timeout path rather than spinning forever.
            self.pull(bus, Line::Clk, false);
            self.pull(bus, Line::Data, false);
        }
    }

    fn enter(&mut self, state: State) {
        self.state = state;
        self.timer = 0;
    }

    /// ATN went low. Every device on the chain answers by pulling `DATA` low,
    /// addressed or not — that is what `$ED47` checks for — and any transfer in
    /// progress is abandoned.
    fn attention(&mut self, bus: &mut Bus) {
        self.under_atn = true;
        self.pull(bus, Line::Clk, false);
        self.pull(bus, Line::Data, true);
        self.lookahead = None;
        self.enter(State::RxWaitClkStart);
    }

    /// ATN went high again: act on whatever we were told.
    fn attention_released<S: Storage>(&mut self, bus: &mut Bus, storage: &mut S) {
        self.under_atn = false;
        match self.role {
            // Turn the bus around and start talking.
            Role::Talker => {
                self.lookahead = storage.read(self.channel);
                self.enter(State::TxTurnaround);
            }
            // Stay a listener: keep holding DATA low and wait for data bytes.
            Role::Listener => self.enter(State::RxWaitClkHigh),
            // Not our conversation — get off the bus.
            Role::Unaddressed => {
                self.pull(bus, Line::Clk, false);
                self.pull(bus, Line::Data, false);
                self.enter(State::Idle);
            }
        }
    }

    /// A complete byte arrived. Under ATN it is a command; otherwise it is data.
    fn byte_received<S: Storage>(&mut self, bus: &mut Bus, storage: &mut S, byte: u8) {
        if !self.under_atn {
            if self.role == Role::Listener {
                if self.opening.is_some() {
                    // Part of a filename.
                    if self.name_len < NAME_MAX {
                        self.name[self.name_len] = byte;
                        self.name_len += 1;
                    }
                } else {
                    storage.write(self.channel, byte);
                }
            }
            return;
        }

        match byte {
            // UNLISTEN. If a name was being collected, the OPEN completes here.
            0x3F => {
                if let Some(ch) = self.opening.take() {
                    let len = self.name_len;
                    // Split the borrow: `name` is ours, `storage` is not.
                    let mut buf = [0u8; NAME_MAX];
                    buf[..len].copy_from_slice(&self.name[..len]);
                    storage.open(ch, &buf[..len]);
                    self.name_len = 0;
                }
                if self.role == Role::Listener {
                    self.role = Role::Unaddressed;
                }
            }
            // UNTALK.
            0x5F => {
                if self.role == Role::Talker {
                    self.role = Role::Unaddressed;
                }
            }
            // LISTEN <device>.
            0x20..=0x3E => {
                self.role = if byte & 0x1F == self.address {
                    Role::Listener
                } else {
                    Role::Unaddressed
                };
            }
            // TALK <device>.
            0x40..=0x5E => {
                self.role = if byte & 0x1F == self.address {
                    Role::Talker
                } else {
                    Role::Unaddressed
                };
            }
            // Secondary address: open an existing channel for data.
            0x60..=0x6F => self.channel = byte & 0x0F,
            // CLOSE a channel.
            0xE0..=0xEF => {
                self.channel = byte & 0x0F;
                storage.close(self.channel);
                self.opening = None;
                self.name_len = 0;
            }
            // OPEN a channel: the filename follows once ATN is released.
            0xF0..=0xFF => {
                self.channel = byte & 0x0F;
                self.opening = Some(self.channel);
                self.name_len = 0;
            }
            _ => {}
        }

        // Note what does *not* happen here: an unaddressed device does not get
        // off the bus. While ATN is asserted every device on the chain keeps
        // receiving and acknowledging command bytes — on a real 1541 that is
        // hardware, an ATN-acknowledge gate holding DATA low for as long as ATN
        // is low, with no say from the DOS. Dropping off happens on the ATN
        // *release* edge, in `attention_released`.
        //
        // This is why `DEVICE NOT PRESENT` for a missing drive surfaces at the
        // first *data* byte rather than at the LISTEN: the LISTEN was answered
        // by everyone, and only once ATN goes high does the line float.
        let _ = bus;
    }

    /// True if the last received byte came with EOI (end of file) flagged.
    pub fn took_eoi(&self) -> bool {
        self.rx_eoi
    }

    /// A short name for the current handshake step, and how long we have been
    /// in it. Purely for debugging — a stalled transfer is almost always "which
    /// line is each side waiting for", and this is how you find out.
    pub fn debug_state(&self) -> (&'static str, u32) {
        let name = match self.state {
            State::Idle => "Idle",
            State::RxWaitClkStart => "RxWaitClkStart",
            State::RxWaitClkHigh => "RxWaitClkHigh",
            State::RxWaitClkLow => "RxWaitClkLow",
            State::RxEoiAck => "RxEoiAck",
            State::RxBitWaitHigh => "RxBitWaitHigh",
            State::RxBitWaitLow => "RxBitWaitLow",
            State::TxTurnaround => "TxTurnaround",
            State::TxReady => "TxReady",
            State::TxEoiStall => "TxEoiStall",
            State::TxEoiAckRelease => "TxEoiAckRelease",
            State::TxStartByte => "TxStartByte",
            State::TxBitSet => "TxBitSet",
            State::TxBitHold => "TxBitHold",
            State::TxWaitAck => "TxWaitAck",
            State::TxDone => "TxDone",
        };
        (name, self.timer)
    }

    /// Which role the controller last assigned: `"unaddressed"`, `"listener"`
    /// or `"talker"`. Debugging aid, as above.
    pub fn debug_role(&self) -> &'static str {
        match self.role {
            Role::Unaddressed => "unaddressed",
            Role::Listener => "listener",
            Role::Talker => "talker",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CONTROLLER;

    /// A storage that hands back a fixed script of bytes.
    struct Fixture {
        data: &'static [u8],
        pos: usize,
        opened: Option<[u8; NAME_MAX]>,
        opened_len: usize,
        written: [u8; 16],
        written_len: usize,
    }

    impl Fixture {
        fn new(data: &'static [u8]) -> Self {
            Fixture {
                data,
                pos: 0,
                opened: None,
                opened_len: 0,
                written: [0; 16],
                written_len: 0,
            }
        }
        fn name(&self) -> &[u8] {
            match &self.opened {
                Some(n) => &n[..self.opened_len],
                None => &[],
            }
        }
    }

    impl Storage for Fixture {
        fn open(&mut self, _channel: u8, name: &[u8]) -> bool {
            let mut buf = [0u8; NAME_MAX];
            let n = name.len().min(NAME_MAX);
            buf[..n].copy_from_slice(&name[..n]);
            self.opened = Some(buf);
            self.opened_len = n;
            self.pos = 0;
            true
        }
        fn read(&mut self, _channel: u8) -> Option<u8> {
            let b = self.data.get(self.pos).copied();
            if b.is_some() {
                self.pos += 1;
            }
            b
        }
        fn write(&mut self, _channel: u8, byte: u8) {
            if self.written_len < self.written.len() {
                self.written[self.written_len] = byte;
                self.written_len += 1;
            }
        }
    }

    /// A minimal stand-in for the KERNAL's side of the bus, so the device can be
    /// exercised without booting a whole C64. It follows the same rules the ROM
    /// does, which is the point: if the two agree, the device is talking
    /// Commodore serial and not some private dialect.
    struct Controller {
        bus: Bus,
    }

    impl Controller {
        fn new() -> Self {
            Controller { bus: Bus::new() }
        }

        fn atn(&mut self, low: bool) {
            self.bus.pull(CONTROLLER, Line::Atn, low);
        }
        fn clk(&mut self, low: bool) {
            self.bus.pull(CONTROLLER, Line::Clk, low);
        }
        fn data(&mut self, low: bool) {
            self.bus.pull(CONTROLLER, Line::Data, low);
        }

        /// Run the device for `cycles`, then return.
        fn run<S: Storage>(&mut self, dev: &mut Device, s: &mut S, cycles: u32) {
            dev.tick(&mut self.bus, s, cycles);
        }

        /// Spin until `pred` holds, up to a budget. Returns false on timeout.
        fn wait<S: Storage, F: Fn(&Bus) -> bool>(
            &mut self,
            dev: &mut Device,
            s: &mut S,
            pred: F,
            budget: u32,
        ) -> bool {
            for _ in 0..budget {
                if pred(&self.bus) {
                    return true;
                }
                dev.tick(&mut self.bus, s, 1);
            }
            pred(&self.bus)
        }

        /// Send one byte, mirroring `$ED40` ISOUR.
        fn send<S: Storage>(&mut self, dev: &mut Device, s: &mut S, byte: u8) -> bool {
            // The device should be holding DATA low; that is "present".
            if self.bus.data() {
                return false;
            }
            self.clk(false); // CLKHI: ready to send
            if !self.wait(dev, s, |b| b.data(), 5_000) {
                return false; // device never said "ready for data"
            }
            self.clk(true); // CLKLO: byte starts
            self.run(dev, s, SETTLE);

            let mut v = byte;
            for _ in 0..8 {
                self.data(v & 1 == 0); // pull low for a zero
                self.run(dev, s, SETTLE);
                self.clk(false); // rising edge: bit valid
                self.run(dev, s, BIT_HOLD);
                self.clk(true);
                self.data(false);
                self.run(dev, s, SETTLE);
                v >>= 1;
            }
            // Wait for the frame acknowledge: DATA pulled low.
            self.wait(dev, s, |b| !b.data(), 5_000)
        }

        /// Receive one byte, mirroring `$EE13` ACPTR. Returns `(byte, eoi)`.
        fn recv<S: Storage>(&mut self, dev: &mut Device, s: &mut S) -> Option<(u8, bool)> {
            self.clk(false);
            if !self.wait(dev, s, |b| b.clk(), 20_000) {
                return None; // talker never said "ready to send"
            }
            self.data(false); // ready for data

            // Time the gap before CLK falls: a long one means EOI.
            let mut eoi = false;
            let mut waited = 0u32;
            while self.bus.clk() {
                dev.tick(&mut self.bus, s, 1);
                waited += 1;
                if waited > 300 && !eoi {
                    // Timer B expired: EOI. Acknowledge with a DATA pulse.
                    eoi = true;
                    self.data(true);
                    self.run(dev, s, EOI_ACK_HOLD);
                    self.data(false);
                }
                if waited > 60_000 {
                    return None;
                }
            }

            let mut value = 0u8;
            for _ in 0..8 {
                if !self.wait(dev, s, |b| b.clk(), 20_000) {
                    return None;
                }
                value >>= 1;
                if self.bus.data() {
                    value |= 0x80;
                }
                if !self.wait(dev, s, |b| !b.clk(), 20_000) {
                    return None;
                }
            }
            self.data(true); // acknowledge the byte
            self.run(dev, s, SETTLE);
            Some((value, eoi))
        }

        /// LISTEN/TALK/etc. — a command byte, sent with ATN asserted.
        fn command<S: Storage>(&mut self, dev: &mut Device, s: &mut S, byte: u8) -> bool {
            self.atn(true);
            self.clk(true);
            self.run(dev, s, SETTLE);
            self.send(dev, s, byte)
        }
    }

    #[test]
    fn pulls_data_low_the_moment_atn_is_asserted() {
        let mut c = Controller::new();
        let mut d = Device::new(DEFAULT_ADDRESS, 1);
        let mut s = Fixture::new(b"");

        assert!(c.bus.data(), "idle bus: DATA floats high");
        c.atn(true);
        c.run(&mut d, &mut s, 1);
        assert!(!c.bus.data(), "a device must answer attention by pulling DATA low");
    }

    #[test]
    fn command_byte_is_received_intact() {
        let mut c = Controller::new();
        let mut d = Device::new(DEFAULT_ADDRESS, 1);
        let mut s = Fixture::new(b"");

        // LISTEN 8 = $20 + 8.
        assert!(c.command(&mut d, &mut s, 0x28), "LISTEN 8 should be acknowledged");
        assert!(d.is_addressed(), "device 8 should now be a listener");
    }

    /// Addressing another device: ours still *acknowledges* the command byte —
    /// under ATN everyone does — but drops off the bus the moment ATN is
    /// released, leaving DATA to float high.
    ///
    /// That two-step is why a missing drive is reported at the first data byte
    /// and not at the LISTEN that addressed it.
    #[test]
    fn another_devices_address_is_acknowledged_then_dropped() {
        let mut c = Controller::new();
        let mut d = Device::new(DEFAULT_ADDRESS, 1);
        let mut s = Fixture::new(b"");

        // LISTEN 9 — not us, but still answered while ATN is low.
        assert!(c.command(&mut d, &mut s, 0x29), "every device answers under ATN");
        assert!(!d.is_addressed(), "device 8 must not take device 9's address");
        assert!(!c.bus.data(), "DATA stays low for as long as ATN is asserted");

        // ATN released: nothing on this chain is addressed, so DATA floats.
        c.atn(false);
        c.run(&mut d, &mut s, SETTLE);
        assert!(c.bus.data(), "an unaddressed device lets go once ATN is high");
    }

    /// The full shape of a `LOAD`: open a channel, send a name, turn the bus
    /// around, and read the file back with EOI on the last byte.
    #[test]
    fn open_then_talk_transfers_a_file() {
        const FILE: &[u8] = &[0x01, 0x08, 0x99, 0x42, 0xFF];
        let mut c = Controller::new();
        let mut d = Device::new(DEFAULT_ADDRESS, 1);
        let mut s = Fixture::new(FILE);

        // LISTEN 8, OPEN channel 0, "TEST", UNLISTEN.
        assert!(c.command(&mut d, &mut s, 0x28));
        assert!(c.command(&mut d, &mut s, 0xF0));
        c.atn(false);
        c.run(&mut d, &mut s, SETTLE);
        for &b in b"TEST" {
            assert!(c.send(&mut d, &mut s, b), "filename byte {b:#04X} was not taken");
        }
        assert!(c.command(&mut d, &mut s, 0x3F)); // UNLISTEN completes the OPEN
        assert_eq!(s.name(), b"TEST", "the device should have collected the name");

        // TALK 8, channel 0, then turn the bus around.
        assert!(c.command(&mut d, &mut s, 0x48));
        assert!(c.command(&mut d, &mut s, 0x60));
        c.data(true); // the C64 becomes a listener: DATALO before releasing ATN
        c.atn(false);
        c.run(&mut d, &mut s, SETTLE);

        let mut got = [0u8; 8];
        let mut n = 0;
        loop {
            let (byte, eoi) = c.recv(&mut d, &mut s).expect("transfer stalled");
            got[n] = byte;
            n += 1;
            if eoi {
                break;
            }
            assert!(n < got.len(), "never saw EOI");
        }
        assert_eq!(&got[..n], FILE, "the file should arrive byte-for-byte");
    }

    #[test]
    fn data_bytes_reach_storage_when_listening() {
        let mut c = Controller::new();
        let mut d = Device::new(DEFAULT_ADDRESS, 1);
        let mut s = Fixture::new(b"");

        // LISTEN 8, channel 2 (not an OPEN, so bytes are data), then send.
        assert!(c.command(&mut d, &mut s, 0x28));
        assert!(c.command(&mut d, &mut s, 0x62));
        c.atn(false);
        c.run(&mut d, &mut s, SETTLE);
        for &b in b"HI" {
            assert!(c.send(&mut d, &mut s, b));
        }
        assert_eq!(&s.written[..s.written_len], b"HI");
    }
}
