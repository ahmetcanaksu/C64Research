//! A headless C64 you can drive from a test.
//!
//! Boot it, type at it, wait for something to appear on screen, read the screen
//! back. Optionally hang a disk drive off its serial bus. No window, no audio,
//! no real time — frames are advanced explicitly, so a test runs as fast as the
//! CPU emulation allows and is deterministic.
//!
//! ```no_run
//! use harness::{d64, Harness};
//!
//! let disk = d64::Disk::new(d64::fixtures::basic_program_image()).unwrap();
//! let mut h = Harness::with_disk(disk).expect("ROMs present");
//! h.boot();
//! h.type_text("load\"*\",8,1\rrun\r");
//! assert!(h.wait_for("HELLO FROM DISK", 2000), "{}", h.screen());
//! ```
//!
//! # Why this exists
//!
//! The interesting bugs in this project are not in one chip, they are in the
//! *seams* — the KERNAL talking to a drive over three wires, a keystroke
//! becoming a matrix scan becoming a BASIC token. Those are only observable
//! from outside the whole machine, and a test that can only inspect registers
//! cannot see them. So: drive the real ROMs and read the screen, exactly as a
//! person would.
//!
//! Requires the C64 ROMs in `roms/` (gitignored — see `roms/README.md`).
//! [`Harness::new`] returns `None` when they are absent so a test can skip
//! rather than fail; [`roms_present`] says whether that will happen.

pub mod keyboard;
pub mod screen;

// Re-exported so a test needs only this one dependency to build a disk, poke
// the bus, or reach into the machine.
pub use c64;
pub use d64;
pub use iec;

use std::collections::VecDeque;
use std::path::PathBuf;

use c64::C64;
use keyboard::{KeyMap, Typist};

/// PAL C64: ~985 kHz / 50 Hz ≈ 19700 CPU cycles per frame.
pub const CYCLES_PER_FRAME: u32 = 19_700;

/// Frames to allow for a cold boot before giving up. A real C64 reaches
/// `READY.` in about two seconds; this is far more than that.
const BOOT_FRAMES: u32 = 400;

/// The workspace's `roms/` directory, resolved at compile time so a test works
/// whatever the current directory is.
fn rom_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../roms")
}

/// The three ROM filenames this project expects.
pub const ROM_FILES: [&str; 3] =
    ["kernal-901227-03.bin", "basic-901226-01.bin", "chargen-901225-01.bin"];

/// Are the C64 ROMs available? If not, tests should skip.
pub fn roms_present() -> bool {
    ROM_FILES.iter().all(|f| rom_dir().join(f).is_file())
}

/// Environment variable that turns a would-be skip into a failure.
pub const REQUIRE_ROMS: &str = "C64_REQUIRE_ROMS";

/// Skip a test with a visible message if the ROMs are missing.
///
/// Use as `if harness::skip_without_roms!("my_test") { return; }`.
///
/// Skipping is right here — the ROMs are Commodore's and can't be committed, so
/// a checkout without them must still build and test. But a skipped test counts
/// as a *passed* test, which is a real reporting hazard: three of this project's
/// tests skipped invisibly for a while, so "56 passed" was quietly covering less
/// than it claimed. Two things guard against that now — the skip says so loudly,
/// and setting `C64_REQUIRE_ROMS=1` makes it a hard failure instead, so a CI job
/// that *does* have the ROMs cannot silently stop exercising them.
#[macro_export]
macro_rules! skip_without_roms {
    ($what:expr) => {{
        if $crate::roms_present() {
            false
        } else if ::std::env::var_os($crate::REQUIRE_ROMS).is_some() {
            panic!(
                "{} needs the C64 ROMs and {} is set: put them in roms/ (see roms/README.md)",
                $what,
                $crate::REQUIRE_ROMS
            );
        } else {
            eprintln!(
                "SKIP {}: C64 ROMs not in roms/ — see roms/README.md ({}=1 to make this fail)",
                $what,
                $crate::REQUIRE_ROMS
            );
            true
        }
    }};
}

/// A disk drive attached to the serial bus: the protocol state machine plus the
/// disk image behind it.
pub struct Drive {
    pub device: iec::Device,
    pub disk: d64::DiskDrive,
}

impl Drive {
    /// A drive at the default address (8) with `disk` in it.
    pub fn new(disk: d64::Disk) -> Self {
        Drive {
            device: iec::Device::new(iec::DEFAULT_ADDRESS, 1),
            disk: d64::DiskDrive::new(disk),
        }
    }

    /// Clock the drive by `cycles`, on the bus it shares with the C64.
    pub fn tick(&mut self, bus: &mut iec::Bus, cycles: u32) {
        self.device.tick(bus, &mut self.disk, cycles);
    }
}

/// A C64, plus everything needed to drive it from a test.
pub struct Harness {
    pub c64: Box<C64>,
    pub drive: Option<Drive>,
    typist: Typist,
    queue: VecDeque<char>,
    char_map: KeyMap,
    /// Frames advanced so far, for diagnostics.
    frames: u32,
}

impl Harness {
    /// Build a machine from the ROMs in `roms/`, or `None` if they're missing.
    pub fn new() -> Option<Self> {
        let dir = rom_dir();
        let read = |f: &str| std::fs::read(dir.join(f)).ok();
        let (kernal, basic, chargen) =
            (read(ROM_FILES[0])?, read(ROM_FILES[1])?, read(ROM_FILES[2])?);
        Some(Harness {
            c64: Box::new(C64::new(&kernal, &basic, &chargen)),
            drive: None,
            typist: Typist::default(),
            queue: VecDeque::new(),
            char_map: keyboard::char_map(),
            frames: 0,
        })
    }

    /// Build a machine with a disk already in a drive on the serial bus.
    pub fn with_disk(disk: d64::Disk) -> Option<Self> {
        let mut h = Self::new()?;
        h.drive = Some(Drive::new(disk));
        Some(h)
    }

    /// Attach a drive (replacing any already there).
    pub fn attach(&mut self, disk: d64::Disk) -> &mut Self {
        self.drive = Some(Drive::new(disk));
        self
    }

    /// Queue text to be typed, one keystroke per few frames. Use `\r` for
    /// RETURN. Queued text is typed by [`Harness::run_frames`] and friends.
    pub fn type_text(&mut self, text: &str) -> &mut Self {
        self.queue.extend(text.chars());
        self
    }

    /// Advance exactly one frame: type a keystroke if one is due, then run a
    /// frame's worth of cycles with the drive clocked alongside.
    pub fn frame(&mut self) {
        self.c64.board.key_matrix = [0; 8];
        self.typist.frame(&mut self.c64, &mut self.queue, &self.char_map);

        let deadline = self.c64.cpu.cycles.wrapping_add(CYCLES_PER_FRAME as u64);
        while self.c64.cpu.cycles < deadline {
            let cycles = self.c64.step();
            if let Some(d) = self.drive.as_mut() {
                d.tick(&mut self.c64.board.iec, cycles as u32);
            }
        }
        self.frames += 1;
    }

    /// Advance `n` frames.
    pub fn run_frames(&mut self, n: u32) -> &mut Self {
        for _ in 0..n {
            self.frame();
        }
        self
    }

    /// Advance until `done` holds, up to `budget` frames. Returns whether it did.
    ///
    /// The predicate is checked between frames *and* is given the machine, so it
    /// can look at RAM as well as the screen — some things (a file's bytes
    /// landing) are only true briefly and are gone by the next frame.
    pub fn run_until<F>(&mut self, budget: u32, mut done: F) -> bool
    where
        F: FnMut(&C64) -> bool,
    {
        for _ in 0..budget {
            if done(&self.c64) {
                return true;
            }
            self.frame();
        }
        done(&self.c64)
    }

    /// Advance up to `budget` frames, checking `watch` after **every CPU
    /// instruction** instead of once per frame, and stopping when it holds.
    ///
    /// Use this for anything on the serial bus. A handshake lasts microseconds —
    /// the KERNAL's ATN assertion is over in a few dozen cycles — and
    /// [`run_until`](Self::run_until) samples once per 19,700-cycle frame, so it
    /// will miss the pulse entirely and report that it never happened. That is a
    /// mistake worth only making once.
    pub fn run_until_step<F>(&mut self, budget: u32, mut watch: F) -> bool
    where
        F: FnMut(&C64) -> bool,
    {
        for _ in 0..budget {
            self.c64.board.key_matrix = [0; 8];
            self.typist.frame(&mut self.c64, &mut self.queue, &self.char_map);

            let deadline = self.c64.cpu.cycles.wrapping_add(CYCLES_PER_FRAME as u64);
            while self.c64.cpu.cycles < deadline {
                let cycles = self.c64.step();
                if let Some(d) = self.drive.as_mut() {
                    d.tick(&mut self.c64.board.iec, cycles as u32);
                }
                if watch(&self.c64) {
                    self.frames += 1;
                    return true;
                }
            }
            self.frames += 1;
        }
        false
    }

    /// Advance until `text` appears on screen, up to `budget` frames.
    pub fn wait_for(&mut self, text: &str, budget: u32) -> bool {
        let want = text.to_string();
        self.run_until(budget, move |c| screen::contains(&c.board.ram, &want))
    }

    /// Run the cold boot through to the `READY.` prompt.
    ///
    /// Panics if it doesn't get there — a machine that won't boot makes every
    /// later assertion meaningless, so failing here with the screen attached is
    /// more useful than failing mysteriously later.
    pub fn boot(&mut self) -> &mut Self {
        assert!(
            self.wait_for("READY.", BOOT_FRAMES),
            "did not reach READY. in {BOOT_FRAMES} frames:\n{}",
            self.screen()
        );
        self
    }

    /// Wait for everything queued to have been typed and consumed by the editor.
    ///
    /// "Consumed" means the KERNAL's keyboard buffer (`$00C6`) is empty as well —
    /// the characters have not just been pressed, they have been read.
    pub fn wait_until_typed(&mut self, budget: u32) -> bool {
        for _ in 0..budget {
            if self.queue.is_empty() && !self.typist.busy() && self.c64.board.ram[0x00C6] == 0 {
                return true;
            }
            self.frame();
        }
        false
    }

    /// The screen as text.
    pub fn screen(&self) -> String {
        screen::text(&self.c64.board.ram)
    }

    /// Does the screen show `text`?
    pub fn screen_contains(&self, text: &str) -> bool {
        screen::contains(&self.c64.board.ram, text)
    }

    /// Characters that had no C64 key and were skipped while typing.
    pub fn dropped_keys(&self) -> &[char] {
        self.typist.dropped()
    }

    /// Frames advanced so far.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// The KERNAL's I/O status byte (`$90`): bit 6 is EOI, bit 7 device-not-present.
    pub fn io_status(&self) -> u8 {
        self.c64.board.ram[0x90]
    }
}
