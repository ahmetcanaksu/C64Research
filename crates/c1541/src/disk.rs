//! The rotating disk under the 1541's read/write head.
//!
//! VIA2 (`$1C00`) is the drive's read/write channel, but the VIA only sees pins
//! — it has no idea a disk is spinning. This module is the mechanism the VIA's
//! pins are wired to: a stack of GCR track images (the [`Disk`]), and the moving
//! parts (the [`Controller`]: head position, motor, stepper, and the byte that
//! is currently under the head).
//!
//! Each step of the drive CPU, [`Controller::tick`] reads the motor and stepper
//! controls the DOS wrote to VIA2 port B, rotates the disk a little, and presents
//! the byte now under the head on port A — plus the two status lines the DOS
//! polls: **SYNC** on PB7 (low while over a sync mark) and **write-protect** on
//! PB4. When a fresh *data* byte arrives it returns `true`: that is the drive's
//! BYTE-READY line, which the real hardware wires to the CPU's SO (set-overflow)
//! pin, so the DOS's tight `BVC` / `CLV` / `LDA $1C01` loop reads exactly one
//! byte per rotation.
//!
//! `no_std` + `alloc`.

use alloc::vec::Vec;
use via6522::Via;

/// Physical tracks on a 1541 disk (1..=35).
const TRACKS: u8 = 35;

/// CPU cycles between bytes under the head, by speed zone.
///
/// The drive spins at a constant 300 rpm, so the outer (lower-numbered) tracks
/// carry more data past the head per second and clock bytes in faster. The DOS
/// selects a matching bit-rate for each zone via VIA2 PB5-6. These are floors —
/// the drive's timing is deliberately approximate here (see the roadmap), enough
/// for the DOS to read, not to satisfy a fast loader counting cycles.
fn cycles_per_byte(track: u8) -> u32 {
    match track {
        1..=17 => 26,
        18..=24 => 28,
        25..=30 => 30,
        _ => 32,
    }
}

/// The media: one byte-aligned GCR bitstream per track, synthesised from a
/// decoded `.d64`.
pub struct Disk {
    /// GCR bitstream per track; index 0 is track 1.
    tracks: Vec<Vec<u8>>,
    write_protected: bool,
}

impl Disk {
    /// Encode every track of a mounted `.d64` into its GCR bitstream — the form
    /// the read head actually sees.
    pub fn from_d64(disk: &d64::Disk) -> Disk {
        let id = disk.id();
        let mut tracks = Vec::with_capacity(TRACKS as usize);
        for track in 1..=TRACKS {
            let n = disk.sectors_on_track(track) as usize;
            let mut sectors = Vec::with_capacity(n);
            for s in 0..n as u8 {
                sectors.push(disk.sector_bytes(track, s).unwrap_or([0; 256]));
            }
            tracks.push(gcr::encode_track(&sectors, track, id));
        }
        Disk { tracks, write_protected: false }
    }

    /// Mark the disk as write-protected (the notch is covered). The DOS senses
    /// this on VIA2 PB4 and refuses to write.
    pub fn set_write_protected(&mut self, protected: bool) {
        self.write_protected = protected;
    }

    /// The GCR bitstream for `track` (1..=35).
    fn stream(&self, track: u8) -> &[u8] {
        &self.tracks[(track - 1) as usize]
    }
}

/// The head, motor and stepper — the parts VIA2's port B drives. Holds the
/// inserted disk (there is nothing to read with an empty drive).
///
/// The all-zero default is a powered-on drive with no disk, head parked at
/// track 1 (`half_track` 0), stepper phase 0.
#[derive(Default)]
pub struct Controller {
    disk: Option<Disk>,
    /// Head position in half-tracks: `track = half_track / 2 + 1`. The stepper
    /// moves in half-tracks; real disks only store data on the whole ones.
    half_track: u8,
    /// Byte index of the head within the current track's GCR stream.
    pos: usize,
    /// The byte last seen under the head, for detecting a run of sync (`$FF`).
    prev_byte: u8,
    /// True while the head sits over a sync mark (drives PB7 low).
    sync: bool,
    /// CPU cycles banked toward the next byte arriving under the head.
    acc: u32,
    /// The stepper phase (PB0-1) last seen, for detecting a step.
    last_phase: u8,
}

impl Controller {
    /// Load a disk into the drive.
    pub fn insert(&mut self, disk: Disk) {
        self.disk = Some(disk);
        self.pos = 0;
        self.prev_byte = 0;
        self.sync = false;
    }

    /// Remove the disk, if any.
    pub fn eject(&mut self) -> Option<Disk> {
        self.disk.take()
    }

    /// Whether a disk is loaded.
    pub fn has_disk(&self) -> bool {
        self.disk.is_some()
    }

    /// The whole track the head is currently over (1..=35).
    pub fn track(&self) -> u8 {
        self.half_track / 2 + 1
    }

    /// True while the head sits over a sync mark (what drives VIA2 PB7 low).
    pub fn syncing(&self) -> bool {
        self.sync
    }

    /// Byte index of the head within the current track — a rotation counter,
    /// useful only for showing that the disk is actually turning.
    pub fn head_pos(&self) -> usize {
        self.pos
    }

    /// Force the head onto a whole track, bypassing a stepper seek. For tests
    /// and for tools that want to read a known track without waiting for the DOS
    /// to bump and seek.
    pub fn seek(&mut self, track: u8) {
        let track = track.clamp(1, TRACKS);
        self.half_track = (track - 1) * 2;
        self.pos = 0;
        self.prev_byte = 0;
        self.sync = false;
        self.acc = 0;
    }

    /// Advance the mechanism by `cycles` CPU clocks.
    ///
    /// Reads the motor (PB2) and stepper (PB0-1) the DOS drives on VIA2 port B,
    /// rotates the disk, and writes the byte under the head to port A along with
    /// SYNC (PB7) and write-protect (PB4) on the port B input pins. Returns
    /// `true` if a fresh data byte became ready — the BYTE-READY pulse the caller
    /// forwards to the CPU's SO pin (V flag).
    pub fn tick(&mut self, via2: &mut Via, cycles: u32) -> bool {
        let pb = via2.port_b();
        let motor_on = pb & 0x04 != 0;

        // Stepper: rotating the two phase bits by one moves the head a half-track.
        //
        // Which way round is not a matter of taste — the DOS decides it, and the
        // ROM says so plainly. `$F326` computes the seek distance as
        // `current_track - wanted_track` (`$22` minus the job's track), then
        // complements it into `$4A` as a signed half-track count; `$FA2E`
        // branches on that sign and **increments** the phase (`INX`) when it is
        // positive. Seeking from track 1 to track 18 leaves `$4A` positive, so:
        //
        //   phase + 1  =>  inward, toward higher track numbers (the disk centre)
        //   phase - 1  =>  outward, toward the rim
        //
        // Get this backwards and a seek walks into the rim stop and stays there,
        // which looks exactly like a head that never moved.
        let phase = pb & 0x03;
        if phase != self.last_phase {
            if phase == (self.last_phase + 1) & 3 {
                if self.half_track < (TRACKS - 1) * 2 {
                    self.half_track += 1;
                }
            } else if phase == (self.last_phase + 3) & 3 && self.half_track > 0 {
                self.half_track -= 1;
            }
            self.last_phase = phase;
            // Landed on a new track: restart at the top of its stream. The DOS
            // re-hunts for a sync mark anyway, so any rotational start is fine.
            self.pos = 0;
            self.prev_byte = 0;
            self.sync = false;
            self.acc = 0;
        }

        let mut byte_ready = false;
        if let (true, Some(disk)) = (motor_on, self.disk.as_ref()) {
            let track = self.half_track / 2 + 1;
            let cpb = cycles_per_byte(track);
            self.acc += cycles;
            // Borrow the current track's stream; the fields we mutate below
            // (`pos`, `prev_byte`, `sync`, `acc`) are all disjoint from `disk`.
            let stream = disk.stream(track);
            while self.acc >= cpb {
                self.acc -= cpb;
                self.pos += 1;
                if self.pos >= stream.len() {
                    self.pos = 0;
                }
                let byte = stream[self.pos];
                via2.pa_in = byte;
                // A sync mark is a run of one-bits; two `$FF` bytes in a row is
                // well past the ten ones the drive's detector needs, and GCR data
                // can never produce it. BYTE-READY is suppressed while syncing —
                // that is what byte-aligns the first data byte after the mark.
                self.sync = byte == 0xFF && self.prev_byte == 0xFF;
                if !self.sync {
                    byte_ready = true;
                }
                self.prev_byte = byte;
            }
        } else {
            self.sync = false;
            self.acc = 0;
        }

        // Present the status lines the DOS polls on port B's input pins.
        let protected = self.disk.as_ref().map(|d| d.write_protected).unwrap_or(false);
        let mut pin = 0xFFu8;
        if self.sync {
            pin &= !0x80; // PB7 low = SYNC detected
        }
        if protected {
            pin &= !0x10; // PB4 low = write-protected
        }
        via2.pb_in = pin;

        byte_ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use via6522::Via;

    /// Build a controller over a synthetic one-file disk, positioned at the
    /// directory track.
    fn drive_on_track(track: u8) -> (Controller, Via) {
        let d = d64::Disk::new(d64::fixtures::synthetic_image()).unwrap();
        let mut ctrl = Controller::default();
        ctrl.insert(Disk::from_d64(&d));
        ctrl.seek(track);
        let mut via2 = Via::new();
        // Real 1541 VIA2 DDRB: PB4 (write-protect) and PB7 (SYNC) are inputs,
        // the rest outputs — so port_b() returns our stepper/motor bits but lets
        // the mechanism drive SYNC and write-protect back in.
        via2.write(0x02, 0x6F); // DDRB
        via2.write(0x00, 0x04); // ORB: motor on
        (ctrl, via2)
    }

    #[test]
    fn stationary_disk_produces_no_bytes() {
        let d = d64::Disk::new(d64::fixtures::synthetic_image()).unwrap();
        let mut ctrl = Controller::default();
        ctrl.insert(Disk::from_d64(&d));
        let mut via2 = Via::new();
        via2.write(0x02, 0x6F);
        via2.write(0x00, 0x00); // motor OFF
        let mut readies = 0;
        for _ in 0..10_000 {
            if ctrl.tick(&mut via2, 4) {
                readies += 1;
            }
        }
        assert_eq!(readies, 0, "a stopped motor must not clock any bytes");
    }

    #[test]
    fn spinning_disk_shows_sync_and_clocks_bytes() {
        let (mut ctrl, mut via2) = drive_on_track(18);
        let mut saw_sync = false;
        let mut readies = 0u32;
        for _ in 0..200_000 {
            let ready = ctrl.tick(&mut via2, 4);
            if ready {
                readies += 1;
            }
            if via2.port_b() & 0x80 == 0 {
                saw_sync = true;
            }
        }
        assert!(saw_sync, "the head should pass over sync marks (PB7 goes low)");
        assert!(readies > 0, "a spinning disk should clock data bytes");
    }

    #[test]
    fn byte_ready_is_suppressed_over_a_sync_mark() {
        // Whenever PB7 reads low (sync), tick must not report a byte ready.
        let (mut ctrl, mut via2) = drive_on_track(18);
        for _ in 0..200_000 {
            let ready = ctrl.tick(&mut via2, 4);
            let syncing = via2.port_b() & 0x80 == 0;
            assert!(!(ready && syncing), "BYTE-READY must be off inside a sync mark");
        }
    }

    #[test]
    fn stepper_moves_the_head_a_half_track_per_phase() {
        let (mut ctrl, mut via2) = drive_on_track(1);
        assert_eq!(ctrl.track(), 1);
        // Inward (toward higher tracks) is a phase *increment* — the direction
        // the DOS's own seek arithmetic implies; see `tick`. Four half-steps in
        // is two whole tracks.
        for phase in [1u8, 2, 3, 0] {
            via2.write(0x00, 0x04 | phase); // keep motor on, set stepper phase
            ctrl.tick(&mut via2, 4);
        }
        assert_eq!(ctrl.track(), 3, "four half-steps in should reach track 3");
        // And back out toward the rim by decrementing it.
        for phase in [3u8, 2, 1, 0] {
            via2.write(0x00, 0x04 | phase);
            ctrl.tick(&mut via2, 4);
        }
        assert_eq!(ctrl.track(), 1, "four half-steps out should return to track 1");
    }
}
