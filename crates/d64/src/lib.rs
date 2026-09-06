//! Reader for `.d64` disk images — the 1541's 35-track, 683-sector format.
//!
//! Parses the directory on track 18 and extracts files by following their
//! track/sector chains, returning the raw `.prg` bytes (load address + data).
//! Used both to load programs directly and, later, to feed the emulated 1541.
//!
//! `no_std` + `alloc`.

#![no_std]
extern crate alloc;

pub mod drive;

pub use drive::{directory_listing, DiskDrive};

use alloc::string::String;
use alloc::vec::Vec;

const SECTOR_SIZE: usize = 256;
const DIR_TRACK: u8 = 18;
const DIR_SECTOR: u8 = 1;

/// Sectors per track (the 1541's zoned layout).
fn sectors_in_track(track: u8) -> u8 {
    match track {
        1..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        _ => 17,
    }
}

/// Byte offset of the first sector of `track` within the image.
fn track_offset(track: u8) -> usize {
    let mut off = 0usize;
    let mut t = 1u8;
    while t < track {
        off += sectors_in_track(t) as usize * SECTOR_SIZE;
        t += 1;
    }
    off
}

fn sector_offset(track: u8, sector: u8) -> usize {
    track_offset(track) + sector as usize * SECTOR_SIZE
}

/// A directory entry.
pub struct DirEntry {
    pub name: String,
    pub file_type: u8, // low nibble: 1=SEQ,2=PRG,3=USR,4=REL
    pub size_sectors: u16,
    first_track: u8,
    first_sector: u8,
}

impl DirEntry {
    pub fn is_prg(&self) -> bool {
        self.file_type & 0x0F == 2
    }
}

/// A disk's identity as stored in its BAM sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// 16-byte PETSCII disk name, padded with `$A0`.
    pub name: [u8; 16],
    /// Two-character disk ID.
    pub id: [u8; 2],
    /// DOS type — `"2A"` for a 1541.
    pub dos_type: [u8; 2],
}

/// A mounted `.d64` image.
pub struct Disk {
    data: Vec<u8>,
}

impl Disk {
    /// Wrap raw `.d64` bytes. Accepts the standard 174848-byte image (and larger
    /// variants that append error info).
    pub fn new(data: Vec<u8>) -> Option<Disk> {
        if data.len() < track_offset(35) + SECTOR_SIZE {
            return None;
        }
        Some(Disk { data })
    }

    fn sector(&self, track: u8, sector: u8) -> &[u8] {
        let off = sector_offset(track, sector);
        &self.data[off..off + SECTOR_SIZE]
    }

    /// Sectors on `track` (the 1541's zoned layout), or 0 for an out-of-range
    /// track. Exposed so a GCR encoder can walk a whole track.
    pub fn sectors_on_track(&self, track: u8) -> u8 {
        if (1..=35).contains(&track) {
            sectors_in_track(track)
        } else {
            0
        }
    }

    /// A copy of one sector's raw 256 decoded bytes, or `None` if the
    /// track/sector is out of range. This is what the emulated drive turns into
    /// a GCR bitstream for its read head.
    pub fn sector_bytes(&self, track: u8, sector: u8) -> Option<[u8; 256]> {
        if !(1..=35).contains(&track) || sector >= sectors_in_track(track) {
            return None;
        }
        let mut out = [0u8; SECTOR_SIZE];
        out.copy_from_slice(self.sector(track, sector));
        Some(out)
    }

    /// The disk's two **raw** ID bytes from the BAM, exactly as they are written
    /// into every sector header on the physical disk.
    ///
    /// Unlike [`Disk::header`], these are not sanitised for display — a sector
    /// header on disk stores the ID verbatim, and the drive's DOS compares the
    /// bytes it reads against these, so the drive must GCR-encode the raw values.
    pub fn id(&self) -> [u8; 2] {
        let bam = self.sector(DIR_TRACK, 0);
        [bam[0xA2], bam[0xA3]]
    }

    /// List every directory entry (closed files with a name).
    pub fn dir(&self) -> Vec<DirEntry> {
        let mut entries = Vec::new();
        let (mut t, mut s) = (DIR_TRACK, DIR_SECTOR);
        // Follow the directory sector chain.
        for _ in 0..64 {
            // guard against a malformed loop
            let sec = self.sector(t, s);
            for e in 0..8usize {
                let b = &sec[e * 32..e * 32 + 32];
                let file_type = b[2];
                if file_type == 0 {
                    continue; // deleted / empty slot
                }
                let name = petscii_name(&b[5..21]);
                entries.push(DirEntry {
                    name,
                    file_type,
                    size_sectors: u16::from_le_bytes([b[30], b[31]]),
                    first_track: b[3],
                    first_sector: b[4],
                });
            }
            let next_t = sec[0];
            if next_t == 0 {
                break;
            }
            t = next_t;
            s = sec[1];
        }
        entries
    }

    /// Extract a file's raw bytes (a `.prg`: load address followed by data) by
    /// following its sector chain.
    pub fn read_entry(&self, entry: &DirEntry) -> Vec<u8> {
        self.read_chain(entry.first_track, entry.first_sector)
    }

    /// Extract the first PRG matching `name` (case-insensitive, ignoring the
    /// C64's `$A0` padding). `"*"` matches the first PRG on the disk.
    pub fn read_prg(&self, name: &str) -> Option<Vec<u8>> {
        let want = name.trim().to_ascii_uppercase();
        self.dir()
            .iter()
            .find(|e| e.is_prg() && (want == "*" || e.name.to_ascii_uppercase() == want))
            .map(|e| self.read_entry(e))
    }

    /// The disk's identity, from the BAM sector (track 18, sector 0).
    ///
    /// Unprintable bytes are replaced, because these go straight into the
    /// synthesised BASIC program of a `$` listing where a `$00` would end the
    /// line early and truncate the directory. *What* they are replaced with
    /// differs between the fields, and the reason is a nice piece of BASIC
    /// trivia:
    ///
    /// - The **name** is padded with `$A0`, the shifted space, exactly as a real
    ///   drive pads it. That works because the name is printed inside quotes,
    ///   and `LIST` does not detokenise inside a quoted string.
    /// - The **ID** and **DOS type** sit *outside* the quotes, where `LIST` does
    ///   detokenise — and `$A0` happens to be the token for `CLOSE`, so padding
    ///   those with `$A0` makes a listing read `01 CLOSE`. They get spaces.
    pub fn header(&self) -> Header {
        let bam = self.sector(DIR_TRACK, 0);
        let mut name = [0xA0u8; 16];
        for (i, slot) in name.iter_mut().enumerate() {
            let b = bam[0x90 + i];
            *slot = if b < 0x20 { 0xA0 } else { b };
        }
        let outside_quotes = |b: u8| if (0x20..0x80).contains(&b) { b } else { b' ' };
        Header {
            name,
            id: [outside_quotes(bam[0xA2]), outside_quotes(bam[0xA3])],
            dos_type: [outside_quotes(bam[0xA5]), outside_quotes(bam[0xA6])],
        }
    }

    /// Free blocks, summed from the BAM's per-track free counts.
    ///
    /// Track 18 is excluded, exactly as a real drive excludes it — the
    /// directory track is not yours to fill, which is why a blank 1541 disk
    /// reports 664 free and not 683.
    pub fn blocks_free(&self) -> u16 {
        let bam = self.sector(DIR_TRACK, 0);
        let mut free = 0u16;
        for track in 1..=35u8 {
            if track == DIR_TRACK {
                continue;
            }
            // Four bytes per track from offset 4; the first is the free count.
            free += bam[4 + (track as usize - 1) * 4] as u16;
        }
        free
    }

    fn read_chain(&self, mut track: u8, mut sector: u8) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..683 {
            // whole-disk guard
            if track == 0 || track > 35 {
                break;
            }
            let sec = self.sector(track, sector);
            let next_t = sec[0];
            let next_s = sec[1];
            if next_t == 0 {
                // Last sector: next_s is the index of the last used byte.
                let used = (next_s as usize).max(2);
                out.extend_from_slice(&sec[2..used + 1]);
                break;
            }
            out.extend_from_slice(&sec[2..SECTOR_SIZE]);
            track = next_t;
            sector = next_s;
        }
        out
    }
}

/// A 16-byte PETSCII directory name, trimmed of the `$A0` shifted-space padding.
fn petscii_name(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        if b == 0xA0 || b == 0x00 {
            break;
        }
        // PETSCII $20..$5F coincides with ASCII; other bytes shown as '?'.
        s.push(if (0x20..=0x5F).contains(&b) { b as char } else { '?' });
    }
    s
}

/// Synthetic disk images, for tests and experiments.
///
/// Hand-built images are how you test a disk format without shipping somebody's
/// copyrighted disk: every byte here is one this crate's own parser has to
/// understand, so a fixture doubles as documentation of the layout.
pub mod fixtures {
    use super::*;

    /// A tiny but valid image: one single-sector PRG named `HELLO`, holding the
    /// load address `$0801` followed by three dummy bytes.
    ///
    /// Deliberately *not* a runnable program — it exists to be transferred and
    /// compared byte-for-byte. If you want a disk that visibly does something,
    /// use [`basic_program_image`].
    pub fn synthetic_image() -> Vec<u8> {
        image_with_prg(b"HELLO", &[0x01, 0x08, 0xAA, 0xBB, 0xCC])
    }

    /// A disk holding a **runnable** BASIC program — `10 PRINT"HELLO FROM DISK"`
    /// — so that `LOAD"*",8,1` followed by `RUN` puts something on the screen.
    ///
    /// This is what a BASIC program looks like on disk, which is worth seeing
    /// once: a two-byte load address, then per line a link to the next line, the
    /// line number, the *tokenised* text (`PRINT` is the single byte `$99`), and
    /// a `$00`; then `$0000` to finish. The link here points at `$0818`, the
    /// address the following line would start at — which is why a BASIC program
    /// is not relocatable without relinking, and why the KERNAL's relocating
    /// load exists.
    pub fn basic_program_image() -> Vec<u8> {
        const TEXT: &[u8] = b"HELLO FROM DISK";
        let mut prg = alloc::vec![0x01, 0x08]; // load address $0801
        // Line 10 starts at $0801; the next line would start after this one.
        let next_line = 0x0801u16 + 2 + 2 + 1 + 1 + TEXT.len() as u16 + 1 + 1;
        prg.extend_from_slice(&next_line.to_le_bytes());
        prg.extend_from_slice(&10u16.to_le_bytes()); // line number 10
        prg.push(0x99); // PRINT
        prg.push(b'"');
        prg.extend_from_slice(TEXT);
        prg.push(b'"');
        prg.push(0x00); // end of line
        prg.extend_from_slice(&[0x00, 0x00]); // end of program
        image_with_prg(b"HELLO", &prg)
    }

    /// Build a `.d64` holding one PRG, named `name`, in a single sector.
    ///
    /// Panics if `prg` needs more than one sector (254 bytes) — these are
    /// fixtures, not a disk writer.
    pub fn image_with_prg(name: &[u8], prg: &[u8]) -> Vec<u8> {
        assert!(prg.len() <= SECTOR_SIZE - 2, "fixture PRGs must fit one sector");
        assert!(name.len() <= 16, "a CBM filename is at most 16 characters");
        let mut data = alloc::vec![0u8; track_offset(35) + SECTOR_SIZE * 17];

        // Put the PRG in sector (1,0). Byte 0 = next track (0 = this is the
        // last sector), byte 1 = index of the last used byte in this sector.
        let s0 = sector_offset(1, 0);
        data[s0] = 0;
        data[s0 + 1] = (prg.len() + 1) as u8;
        data[s0 + 2..s0 + 2 + prg.len()].copy_from_slice(prg);

        // Make track 18 sector 0 a believable BAM: a disk name, an ID, the
        // DOS type a 1541 writes, and a per-track free count for every track
        // but the directory track itself. Without this the image looks
        // unformatted, and a `$` listing of it is misleading.
        let bam = sector_offset(DIR_TRACK, 0);
        data[bam] = DIR_TRACK; // first directory sector: 18/1
        data[bam + 1] = DIR_SECTOR;
        data[bam + 2] = b'A'; // DOS version
        for track in 1..=35u8 {
            let mut free = sectors_in_track(track);
            if track == DIR_TRACK {
                free = 0; // the directory track is not available
            } else if track == 1 {
                free -= 1; // the one sector our PRG occupies
            }
            data[bam + 4 + (track as usize - 1) * 4] = free;
        }
        let disk_name = b"TEST DISK";
        for i in 0..16 {
            data[bam + 0x90 + i] = disk_name.get(i).copied().unwrap_or(0xA0);
        }
        data[bam + 0xA0] = 0xA0;
        data[bam + 0xA1] = 0xA0;
        data[bam + 0xA2] = b'0'; // disk ID "01"
        data[bam + 0xA3] = b'1';
        data[bam + 0xA4] = 0xA0;
        data[bam + 0xA5] = b'2'; // DOS type "2A"
        data[bam + 0xA6] = b'A';

        // Directory entry on track 18 sector 1.
        let d = sector_offset(DIR_TRACK, DIR_SECTOR);
        data[d] = 0; // no next dir sector
        data[d + 1] = 0xFF;
        data[d + 2] = 0x82; // closed PRG
        data[d + 3] = 1; // first track
        data[d + 4] = 0; // first sector
        data[d + 5..d + 5 + name.len()].copy_from_slice(name);
        data[d + 5 + name.len()..d + 21].fill(0xA0); // shifted-space padding
        data[d + 30] = 1; // size in sectors

        data
    }

    /// [`synthetic_image`], mounted.
    pub fn synthetic_disk() -> Disk {
        Disk::new(synthetic_image()).expect("the fixture is a valid image")
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::synthetic_disk;
    use super::*;

    #[test]
    fn lists_the_directory() {
        let disk = synthetic_disk();
        let dir = disk.dir();
        assert_eq!(dir.len(), 1);
        assert_eq!(dir[0].name, "HELLO");
        assert!(dir[0].is_prg());
        assert_eq!(dir[0].size_sectors, 1);
    }

    #[test]
    fn extracts_the_prg_across_sectors() {
        let disk = synthetic_disk();
        let prg = disk.read_prg("hello").unwrap();
        // Load address $0801, then $AA (sector 0) + $BB,$CC (sector 1).
        assert_eq!(prg, alloc::vec![0x01, 0x08, 0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn wildcard_matches_first_prg() {
        let disk = synthetic_disk();
        assert!(disk.read_prg("*").is_some());
    }
}
