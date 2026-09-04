//! A `.d64` image behind the serial-bus [`Storage`](iec::Storage) interface —
//! the disk half of a disk drive, with none of the mechanism.
//!
//! [`crate::Disk`] already knows how to read the directory and follow a file's
//! sector chain. This adds the part a C64 actually talks to: filename parsing
//! (including `*` and `?` patterns), and the `$` directory, which on a real
//! drive is not a file at all but a **BASIC program synthesised on the fly** so
//! that `LOAD"$",8` + `LIST` shows you a listing.
//!
//! What is deliberately missing: the command/error channel (secondary address
//! 15), writing, and anything to do with tracks, sectors, GCR or the head. A
//! real 1541 reaches its bytes by stepping a motor; this reaches them with an
//! array index. Closing that gap is what wiring the drive's own firmware to the
//! bus is for.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Disk;

/// How many secondary addresses a device has.
const CHANNELS: usize = 16;

/// The command/error channel. Secondary address 15 is not a file: writing to it
/// issues a DOS command, and reading it returns the drive's status line.
pub const COMMAND_CHANNEL: u8 = 15;

/// The drive's current status, as the error channel reports it.
///
/// A 1541 answers with `code,MESSAGE,track,sector` followed by a carriage
/// return — always in that shape, which is why `INPUT#15,A,B$,C,D` is the
/// idiomatic way to read it. Two behaviours matter and are easy to miss:
///
/// - **Reading the channel clears it.** After you read an error, the drive
///   reverts to `00, OK,00,00`. Programs rely on that to tell a fresh error from
///   a stale one.
/// - **A freshly powered drive reports `73`**, its DOS version banner, not `00`.
///   Getting `73` back is how you know nobody has talked to it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DosStatus {
    pub code: u8,
    pub message: &'static str,
    pub track: u8,
    pub sector: u8,
}

impl DosStatus {
    /// `00, OK,00,00` — nothing wrong.
    pub const OK: DosStatus = DosStatus { code: 0, message: "OK", track: 0, sector: 0 };
    /// The power-on banner a 1541 reports until it is initialised.
    pub const POWER_ON: DosStatus =
        DosStatus { code: 73, message: "CBM DOS V2.6 1541", track: 0, sector: 0 };
    /// `62,FILE NOT FOUND` — the open failed.
    pub const FILE_NOT_FOUND: DosStatus =
        DosStatus { code: 62, message: "FILE NOT FOUND", track: 0, sector: 0 };
    /// `31,SYNTAX ERROR` — the command wasn't understood.
    pub const SYNTAX_ERROR: DosStatus =
        DosStatus { code: 31, message: "SYNTAX ERROR", track: 0, sector: 0 };
    /// `26,WRITE PROTECT ON` — this drive is read-only.
    pub const WRITE_PROTECT: DosStatus =
        DosStatus { code: 26, message: "WRITE PROTECT ON", track: 0, sector: 0 };
    /// `74,DRIVE NOT READY` — no disk.
    pub const NOT_READY: DosStatus =
        DosStatus { code: 74, message: "DRIVE NOT READY", track: 0, sector: 0 };

    /// The exact bytes the drive sends: `NN,MESSAGE,TT,SS` + CR.
    pub fn to_line(self) -> Vec<u8> {
        let mut out = Vec::new();
        let two = |n: u8, out: &mut Vec<u8>| {
            out.push(b'0' + (n / 10) % 10);
            out.push(b'0' + n % 10);
        };
        two(self.code, &mut out);
        out.push(b',');
        out.extend_from_slice(self.message.as_bytes());
        out.push(b',');
        two(self.track, &mut out);
        out.push(b',');
        two(self.sector, &mut out);
        out.push(b'\r');
        out
    }
}

/// One open channel: a byte stream and how far the C64 has read.
#[derive(Default, Clone)]
struct Channel {
    data: Vec<u8>,
    pos: usize,
}

/// A disk image serving files over the serial bus.
pub struct DiskDrive {
    disk: Option<Disk>,
    channels: Vec<Channel>,
    /// Name the last [`open`](iec::Storage::open) was asked for, for tests and
    /// for a host that wants to report what the C64 is doing.
    last_name: String,
    last_open_ok: bool,
    /// What the error channel will report next.
    status: DosStatus,
    /// Bytes of a DOS command arriving on channel 15, until its carriage return.
    command: Vec<u8>,
}

impl DiskDrive {
    /// A drive with `disk` in it.
    pub fn new(disk: Disk) -> Self {
        DiskDrive {
            disk: Some(disk),
            channels: alloc::vec![Channel::default(); CHANNELS],
            last_name: String::new(),
            last_open_ok: false,
            status: DosStatus::POWER_ON,
            command: Vec::new(),
        }
    }

    /// A drive with no disk in it. Every open fails, as it should.
    pub fn empty() -> Self {
        DiskDrive {
            disk: None,
            channels: alloc::vec![Channel::default(); CHANNELS],
            last_name: String::new(),
            last_open_ok: false,
            status: DosStatus::POWER_ON,
            command: Vec::new(),
        }
    }

    /// Put a disk in (or take one out with `None`).
    pub fn insert(&mut self, disk: Option<Disk>) {
        self.disk = disk;
        for c in &mut self.channels {
            *c = Channel::default();
        }
    }

    /// The filename of the most recent open attempt, and whether it succeeded.
    pub fn last_open(&self) -> (&str, bool) {
        (&self.last_name, self.last_open_ok)
    }

    /// What the error channel would report right now, without consuming it.
    pub fn status(&self) -> DosStatus {
        self.status
    }

    /// Run a DOS command, as sent to channel 15.
    ///
    /// This drive is read-only, so the commands that would change a disk answer
    /// `26,WRITE PROTECT ON` rather than pretending to work — an honest refusal
    /// is far easier to debug than a silent no-op.
    fn execute(&mut self, command: &[u8]) {
        let text: String = command
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_ascii_uppercase();

        self.status = match text.as_bytes().first() {
            // Nothing to do, but a command channel opened with no command is
            // how a program asks for the status, so leave it alone.
            None => return,
            // INITIALIZE / UI, UJ (reset): re-read the BAM and clear the error.
            Some(b'I') | Some(b'U') => {
                if self.disk.is_some() {
                    DosStatus::OK
                } else {
                    DosStatus::NOT_READY
                }
            }
            // VALIDATE: nothing to collect on a read-only disk, so it succeeds.
            Some(b'V') => DosStatus::OK,
            // Anything that writes: NEW (format), SCRATCH, RENAME, COPY.
            Some(b'N') | Some(b'S') | Some(b'R') | Some(b'C') => DosStatus::WRITE_PROTECT,
            _ => DosStatus::SYNTAX_ERROR,
        };
    }

    /// PETSCII filename bytes -> an uppercase Rust string, stopping at the
    /// first `,` (a CBM name can carry `,P,R` type/mode options).
    ///
    /// PETSCII is not ASCII, and the overlap is only partial: `$20`-`$5F`
    /// coincide, the *unshifted* letters a C64 sends when you type a name live
    /// at `$41`-`$5A`, and the shifted set lives up at `$C1`-`$DA`. Fold that
    /// upper range back down and uppercase the lot, so a name compares equal
    /// however it was typed.
    fn parse_name(raw: &[u8]) -> String {
        let mut s = String::new();
        for &b in raw {
            if b == b',' {
                break;
            }
            let c = match b {
                0x20..=0x7E => b,
                0xC1..=0xDA => b - 0x80, // shifted letters -> $41..$5A
                _ => continue,
            };
            s.push(c as char);
        }
        s.trim().to_ascii_uppercase()
    }

    /// CBM pattern matching: `*` matches the rest of the name, `?` any single
    /// character. `"*"` alone therefore matches the first file on the disk.
    fn matches(pattern: &str, name: &str) -> bool {
        let (p, n) = (pattern.as_bytes(), name.as_bytes());
        let mut i = 0;
        while i < p.len() {
            match p[i] {
                b'*' => return true, // everything from here on matches
                b'?' if i < n.len() => {}
                c if i < n.len() && n[i] == c => {}
                _ => return false,
            }
            i += 1;
        }
        i == n.len()
    }

    /// Find a PRG by CBM pattern and return its raw bytes.
    fn find_prg(&self, pattern: &str) -> Option<Vec<u8>> {
        let disk = self.disk.as_ref()?;
        disk.dir()
            .iter()
            .find(|e| e.is_prg() && Self::matches(pattern, &e.name.to_ascii_uppercase()))
            .map(|e| disk.read_entry(e))
    }
}

impl iec::Storage for DiskDrive {
    fn open(&mut self, channel: u8, name: &[u8]) -> bool {
        // Channel 15 is the command channel, never a file. `OPEN 15,8,15,"I0"`
        // arrives here with the command as the "filename".
        if channel == COMMAND_CHANNEL {
            self.execute(name);
            self.channels[COMMAND_CHANNEL as usize] = Channel::default();
            return true;
        }

        let pattern = Self::parse_name(name);
        self.last_name = pattern.clone();

        let data = if pattern.starts_with('$') {
            self.disk.as_ref().map(directory_listing)
        } else if pattern.is_empty() {
            None
        } else {
            self.find_prg(&pattern)
        };

        self.last_open_ok = data.is_some();
        // Record *why* it failed, so a program asking the error channel gets a
        // real answer instead of silence.
        if !self.last_open_ok {
            self.status = if self.disk.is_none() {
                DosStatus::NOT_READY
            } else {
                DosStatus::FILE_NOT_FOUND
            };
        }
        let ch = (channel as usize) % CHANNELS;
        self.channels[ch] = Channel { data: data.unwrap_or_default(), pos: 0 };
        self.last_open_ok
    }

    fn read(&mut self, channel: u8) -> Option<u8> {
        if channel == COMMAND_CHANNEL {
            let ch = &mut self.channels[COMMAND_CHANNEL as usize];
            if ch.data.is_empty() {
                // First read of this status: render it now.
                ch.data = self.status.to_line();
                ch.pos = 0;
            }
            let b = ch.data.get(ch.pos).copied();
            match b {
                Some(_) => ch.pos += 1,
                // Fully read: the drive reverts to OK, as a real one does.
                None => {
                    self.status = DosStatus::OK;
                    self.channels[COMMAND_CHANNEL as usize] = Channel::default();
                }
            }
            return b;
        }

        let ch = &mut self.channels[(channel as usize) % CHANNELS];
        let b = ch.data.get(ch.pos).copied();
        if b.is_some() {
            ch.pos += 1;
        }
        b
    }

    fn write(&mut self, channel: u8, byte: u8) {
        if channel != COMMAND_CHANNEL {
            return; // read-only: file writes go nowhere
        }
        // A command ends at its carriage return, which is what `PRINT#15` sends.
        if byte == b'\r' {
            let cmd = core::mem::take(&mut self.command);
            self.execute(&cmd);
        } else if self.command.len() < 64 {
            self.command.push(byte);
        }
    }

    fn close(&mut self, channel: u8) {
        if channel == COMMAND_CHANNEL {
            // Closing the command channel runs anything not yet terminated.
            if !self.command.is_empty() {
                let cmd = core::mem::take(&mut self.command);
                self.execute(&cmd);
            }
        }
        self.channels[(channel as usize) % CHANNELS] = Channel::default();
    }
}

/// Build the fake BASIC program a drive sends for `LOAD"$",8`.
///
/// This is one of the C64's better jokes: the directory is not text and not a
/// file, it is a **program**. The drive fabricates something with the exact
/// shape BASIC expects in memory, so `LIST` renders it:
///
/// ```text
///   $0401              load address (BASIC's start on a PET; any page works)
///   per line:
///     2 bytes   link to the next line — only "nonzero" matters
///     2 bytes   line number, little-endian — reused as the block count
///     n bytes   the text of the line, in PETSCII
///     1 byte    $00, ending the line
///   2 bytes    $0000, ending the program
/// ```
///
/// So the block counts down the left-hand side of a directory listing are BASIC
/// *line numbers*, and `LIST` is what formats them. Nothing about this is a
/// filesystem operation.
pub fn directory_listing(disk: &Disk) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x01, 0x04]); // load address $0401

    let header = disk.header();
    let line = |number: u16, text: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&[0x01, 0x01]); // any nonzero link
        out.extend_from_slice(&number.to_le_bytes());
        out.extend_from_slice(text);
        out.push(0x00);
    };

    // Line 0: the reverse-video header — 0x12 is RVS ON.
    let mut head = Vec::new();
    head.push(0x12);
    head.push(b'"');
    head.extend_from_slice(&header.name);
    head.push(b'"');
    head.push(b' ');
    head.extend_from_slice(&header.id);
    head.push(b' ');
    head.extend_from_slice(&header.dos_type);
    line(0, &head, &mut out);

    // One line per entry.
    for e in disk.dir() {
        let mut text = Vec::new();
        // Pad so the quoted names line up under each other once LIST has
        // printed the block count as a line number.
        let pad = match e.size_sectors {
            0..=9 => 3,
            10..=99 => 2,
            _ => 1,
        };
        text.resize(pad, b' ');
        text.push(b'"');
        text.extend_from_slice(e.name.as_bytes());
        text.push(b'"');
        // Name field is 18 columns wide including both quotes.
        let quoted = e.name.len() + 2;
        text.resize(text.len() + 18usize.saturating_sub(quoted), b' ');
        text.extend_from_slice(file_type_name(e.file_type));
        line(e.size_sectors, &text, &mut out);
    }

    line(disk.blocks_free(), b"BLOCKS FREE.", &mut out);
    out.extend_from_slice(&[0x00, 0x00]); // end of program
    out
}

/// The three-letter type a directory listing shows.
fn file_type_name(file_type: u8) -> &'static [u8] {
    match file_type & 0x0F {
        0 => b"DEL",
        1 => b"SEQ",
        2 => b"PRG",
        3 => b"USR",
        4 => b"REL",
        _ => b"???",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iec::Storage;

    fn drive() -> DiskDrive {
        DiskDrive::new(crate::fixtures::synthetic_disk())
    }

    #[test]
    fn matches_cbm_patterns() {
        assert!(DiskDrive::matches("*", "ANYTHING"));
        assert!(DiskDrive::matches("HELLO", "HELLO"));
        assert!(DiskDrive::matches("HEL*", "HELLO"));
        assert!(DiskDrive::matches("H?LLO", "HELLO"));
        assert!(!DiskDrive::matches("HELL", "HELLO"), "no implicit prefix match");
        assert!(!DiskDrive::matches("XELLO", "HELLO"));
    }

    #[test]
    fn strips_options_and_uppercases_the_name() {
        assert_eq!(DiskDrive::parse_name(b"hello,p,r"), "HELLO");
        assert_eq!(DiskDrive::parse_name(b"  test  "), "TEST");
    }

    #[test]
    fn opens_a_prg_and_streams_it() {
        let mut d = drive();
        assert!(d.open(0, b"HELLO"));
        let mut got = Vec::new();
        while let Some(b) = d.read(0) {
            got.push(b);
        }
        assert_eq!(got, alloc::vec![0x01, 0x08, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn wildcard_opens_the_first_prg() {
        let mut d = drive();
        assert!(d.open(1, b"*"));
        assert_eq!(d.read(1), Some(0x01));
        assert_eq!(d.last_open(), ("*", true));
    }

    #[test]
    fn a_missing_file_fails_to_open_and_streams_nothing() {
        let mut d = drive();
        assert!(!d.open(0, b"NOPE"));
        assert_eq!(d.read(0), None);
    }

    #[test]
    fn an_empty_drive_opens_nothing() {
        let mut d = DiskDrive::empty();
        assert!(!d.open(0, b"$"));
        assert!(!d.open(0, b"*"));
    }

    /// The `$` listing has to be a well-formed BASIC program: right load
    /// address, nonzero line links, and a `$0000` terminator.
    #[test]
    fn directory_is_a_valid_basic_program() {
        let mut d = drive();
        assert!(d.open(0, b"$"));
        let mut img = Vec::new();
        while let Some(b) = d.read(0) {
            img.push(b);
        }

        assert_eq!(&img[..2], &[0x01, 0x04], "loads at $0401");
        assert_eq!(&img[img.len() - 2..], &[0x00, 0x00], "ends with a null link");
        assert_eq!(&img[2..4], &[0x01, 0x01], "first line has a nonzero link");
        assert_eq!(img[6], 0x12, "header line starts with RVS ON");

        // The filename and its type should both appear in the rendered text.
        let text = String::from_utf8_lossy(&img).to_ascii_uppercase();
        assert!(text.contains("HELLO"), "listing should name the file: {text}");
        assert!(text.contains("PRG"), "listing should show the type");
        assert!(text.contains("BLOCKS FREE."), "listing should end with the free count");
    }

    #[test]
    fn closing_a_channel_forgets_it() {
        let mut d = drive();
        assert!(d.open(2, b"HELLO"));
        assert!(d.read(2).is_some());
        d.close(2);
        assert_eq!(d.read(2), None);
    }
}

#[cfg(test)]
mod command_channel_tests {
    use super::*;
    use iec::Storage;

    fn drive() -> DiskDrive {
        DiskDrive::new(crate::fixtures::synthetic_disk())
    }

    fn read_all(d: &mut DiskDrive, channel: u8) -> String {
        let mut s = String::new();
        while let Some(b) = d.read(channel) {
            s.push(b as char);
        }
        s
    }

    #[test]
    fn status_line_has_the_shape_input_expects() {
        assert_eq!(
            String::from_utf8(DosStatus::FILE_NOT_FOUND.to_line()).unwrap(),
            "62,FILE NOT FOUND,00,00\r"
        );
        assert_eq!(String::from_utf8(DosStatus::OK.to_line()).unwrap(), "00,OK,00,00\r");
    }

    /// A drive nobody has spoken to reports its DOS banner, not `00, OK`.
    #[test]
    fn a_fresh_drive_reports_its_dos_version() {
        let mut d = drive();
        assert_eq!(read_all(&mut d, COMMAND_CHANNEL), "73,CBM DOS V2.6 1541,00,00\r");
    }

    /// Reading the error channel clears it — that is how a program distinguishes
    /// a new error from one it has already seen.
    #[test]
    fn reading_the_status_clears_it() {
        let mut d = drive();
        let _ = read_all(&mut d, COMMAND_CHANNEL);
        assert_eq!(read_all(&mut d, COMMAND_CHANNEL), "00,OK,00,00\r");
    }

    #[test]
    fn a_failed_open_is_reported_on_the_error_channel() {
        let mut d = drive();
        assert!(!d.open(0, b"NOPE"));
        assert_eq!(d.status(), DosStatus::FILE_NOT_FOUND);
        assert_eq!(read_all(&mut d, COMMAND_CHANNEL), "62,FILE NOT FOUND,00,00\r");
        // ...and is cleared once read.
        assert_eq!(d.status(), DosStatus::OK);
    }

    #[test]
    fn a_successful_open_leaves_the_status_alone() {
        let mut d = drive();
        let _ = read_all(&mut d, COMMAND_CHANNEL); // clear the power-on banner
        assert!(d.open(0, b"HELLO"));
        assert_eq!(d.status(), DosStatus::OK);
    }

    #[test]
    fn initialize_clears_an_error() {
        let mut d = drive();
        assert!(!d.open(0, b"NOPE"));
        d.open(COMMAND_CHANNEL, b"I0");
        assert_eq!(d.status(), DosStatus::OK);
    }

    /// Commands arrive a byte at a time from `PRINT#15` and end at the CR.
    #[test]
    fn a_command_written_byte_by_byte_runs_at_the_carriage_return() {
        let mut d = drive();
        assert!(!d.open(0, b"NOPE"));
        for &b in b"I0" {
            d.write(COMMAND_CHANNEL, b);
        }
        assert_eq!(d.status(), DosStatus::FILE_NOT_FOUND, "not run until the CR");
        d.write(COMMAND_CHANNEL, b'\r');
        assert_eq!(d.status(), DosStatus::OK);
    }

    /// Read-only means read-only, and says so.
    #[test]
    fn write_commands_are_refused_honestly() {
        let mut d = drive();
        for cmd in [b"N:BLANK,01".as_slice(), b"S:HELLO", b"R:A=B", b"C:A=B"] {
            d.open(COMMAND_CHANNEL, cmd);
            assert_eq!(
                d.status(),
                DosStatus::WRITE_PROTECT,
                "{:?} should be refused, not silently ignored",
                core::str::from_utf8(cmd).unwrap()
            );
        }
    }

    #[test]
    fn nonsense_is_a_syntax_error() {
        let mut d = drive();
        d.open(COMMAND_CHANNEL, b"WOBBLE");
        assert_eq!(d.status(), DosStatus::SYNTAX_ERROR);
    }

    #[test]
    fn an_empty_drive_says_it_is_not_ready() {
        let mut d = DiskDrive::empty();
        d.open(COMMAND_CHANNEL, b"I0");
        assert_eq!(d.status(), DosStatus::NOT_READY);
        assert!(!d.open(0, b"HELLO"));
        assert_eq!(d.status(), DosStatus::NOT_READY);
    }

    /// File writes are dropped, but must not be mistaken for commands.
    #[test]
    fn writing_to_a_file_channel_does_not_run_commands() {
        let mut d = drive();
        let before = d.status();
        for &b in b"N:WIPE\r" {
            d.write(2, b);
        }
        assert_eq!(d.status(), before, "a data channel is not the command channel");
    }
}
