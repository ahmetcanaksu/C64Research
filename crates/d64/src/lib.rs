//! Reader for `.d64` disk images — the 1541's 35-track, 683-sector format.
//!
//! Parses the directory on track 18 and extracts files by following their
//! track/sector chains, returning the raw `.prg` bytes (load address + data).
//! Used both to load programs directly and, later, to feed the emulated 1541.
//!
//! `no_std` + `alloc`.

#![no_std]
extern crate alloc;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny synthetic disk with one PRG file to exercise the parser.
    fn synthetic_disk() -> Disk {
        let mut data = alloc::vec![0u8; track_offset(35) + SECTOR_SIZE * 17];

        // Put a single-sector PRG on track 1: load addr $0801, then 3 data bytes.
        // Sector (1,0) is the last sector; byte 1 = index of the last used byte.
        let s0 = sector_offset(1, 0);
        data[s0] = 0; // no next track -> last sector
        data[s0 + 1] = 6; // last used byte index (bytes 2..=6 are valid)
        data[s0 + 2] = 0x01; // load lo ($0801)
        data[s0 + 3] = 0x08; // load hi
        data[s0 + 4] = 0xAA;
        data[s0 + 5] = 0xBB;
        data[s0 + 6] = 0xCC;

        // Directory entry on track 18 sector 1.
        let d = sector_offset(DIR_TRACK, DIR_SECTOR);
        data[d] = 0; // no next dir sector
        data[d + 1] = 0xFF;
        data[d + 2] = 0x82; // closed PRG
        data[d + 3] = 1; // first track
        data[d + 4] = 0; // first sector
        let name = b"HELLO";
        data[d + 5..d + 5 + name.len()].copy_from_slice(name);
        for i in d + 5 + name.len()..d + 21 {
            data[i] = 0xA0; // pad
        }
        data[d + 30] = 1; // size in sectors

        Disk::new(data).unwrap()
    }

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
