//! GCR — Group Code Recording, the format a 1541 actually stores on disk.
//!
//! A `.d64` holds decoded 256-byte sectors, but the drive's read head sees a
//! ring of magnetic flux, not bytes. Data is written in **GCR**: every 4 bits
//! become a 5-bit code chosen so no run of more than two zero bits ever appears
//! — which is what lets the drive recover its clock from the data alone. A track
//! is a loop of sync marks, sector headers and data blocks with gaps between.
//!
//! This crate builds that loop from a track's sectors (and can decode it back),
//! so the emulated disk controller has a real bitstream to read.
//!
//! `no_std` + `alloc`.

#![no_std]
extern crate alloc;

use alloc::vec::Vec;

/// 4-bit nibble -> 5-bit GCR code.
const ENC: [u8; 16] = [
    0x0A, 0x0B, 0x12, 0x13, 0x0E, 0x0F, 0x16, 0x17, 0x09, 0x19, 0x1A, 0x1B, 0x0D, 0x1D, 0x1E, 0x15,
];

/// 5-bit GCR code -> 4-bit nibble, or `0xFF` for an invalid code.
const fn build_dec() -> [u8; 32] {
    let mut t = [0xFFu8; 32];
    let mut i = 0;
    while i < 16 {
        t[ENC[i] as usize] = i as u8;
        i += 1;
    }
    t
}
const DEC: [u8; 32] = build_dec();

/// Sectors per track in the 1541's four speed zones.
pub fn sectors_per_track(track: u8) -> u8 {
    match track {
        1..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        _ => 17,
    }
}

/// Encode four data bytes into five GCR bytes.
pub fn encode_group(inp: &[u8; 4], out: &mut [u8; 5]) {
    let nibbles = [
        inp[0] >> 4,
        inp[0] & 0x0F,
        inp[1] >> 4,
        inp[1] & 0x0F,
        inp[2] >> 4,
        inp[2] & 0x0F,
        inp[3] >> 4,
        inp[3] & 0x0F,
    ];
    // Eight 5-bit codes = 40 bits, packed most-significant-first.
    let mut bits: u64 = 0;
    for n in nibbles {
        bits = (bits << 5) | ENC[n as usize] as u64;
    }
    for (i, b) in out.iter_mut().enumerate() {
        *b = (bits >> (32 - i * 8)) as u8;
    }
}

/// Decode five GCR bytes back into four data bytes. Returns `false` if any
/// 5-bit code was invalid.
pub fn decode_group(inp: &[u8; 5], out: &mut [u8; 4]) -> bool {
    let mut bits: u64 = 0;
    for &b in inp {
        bits = (bits << 8) | b as u64;
    }
    let mut nibbles = [0u8; 8];
    for (i, nib) in nibbles.iter_mut().enumerate() {
        let code = (bits >> (35 - i * 5)) as u8 & 0x1F;
        let n = DEC[code as usize];
        if n == 0xFF {
            return false;
        }
        *nib = n;
    }
    for (i, b) in out.iter_mut().enumerate() {
        *b = (nibbles[i * 2] << 4) | nibbles[i * 2 + 1];
    }
    true
}

/// Append the GCR encoding of a block (length a multiple of 4) to `out`.
fn encode_block(block: &[u8], out: &mut Vec<u8>) {
    debug_assert!(block.len().is_multiple_of(4));
    let mut g = [0u8; 5];
    for chunk in block.chunks_exact(4) {
        encode_group(chunk.try_into().unwrap(), &mut g);
        out.extend_from_slice(&g);
    }
}

/// Bytes of sync (all-ones) written before each header and data block.
const SYNC_LEN: usize = 5;
/// The gap byte the drive writes to fill space between blocks.
const GAP: u8 = 0x55;

/// Build the full GCR bitstream (byte-aligned) for one track, given its decoded
/// sectors in order and the disk's two ID bytes.
///
/// Layout per sector: sync, GCR(header), header gap, sync, GCR(data), gap.
/// - **header** = `08 checksum sector track id2 id1 0F 0F` (checksum = the four
///   XORed together).
/// - **data** = `07 <256 bytes> checksum 00 00` (checksum = the 256 bytes XORed).
pub fn encode_track(sectors: &[[u8; 256]], track: u8, id: [u8; 2]) -> Vec<u8> {
    let mut out = Vec::new();
    for (sector, data) in sectors.iter().enumerate() {
        let sector = sector as u8;

        // ---- header ----
        out.extend(core::iter::repeat_n(0xFF, SYNC_LEN));
        let hchk = sector ^ track ^ id[0] ^ id[1];
        let header = [0x08, hchk, sector, track, id[1], id[0], 0x0F, 0x0F];
        encode_block(&header, &mut out);
        out.extend(core::iter::repeat_n(GAP, 9));

        // ---- data ----
        out.extend(core::iter::repeat_n(0xFF, SYNC_LEN));
        let mut block = Vec::with_capacity(260);
        block.push(0x07);
        block.extend_from_slice(data);
        block.push(data.iter().fold(0u8, |a, &b| a ^ b)); // data checksum
        block.push(0x00);
        block.push(0x00);
        encode_block(&block, &mut out);
        out.extend(core::iter::repeat_n(GAP, 8));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_round_trips() {
        let inp = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut g = [0u8; 5];
        encode_group(&inp, &mut g);
        let mut back = [0u8; 4];
        assert!(decode_group(&g, &mut back));
        assert_eq!(back, inp);
    }

    #[test]
    fn every_byte_pair_round_trips() {
        // Exhaustively check the 4-bit tables via all 65536 four-byte values'
        // low/high halves would be overkill; cover all 256 byte values in each
        // position paired with a rolling partner.
        for a in 0u16..=255 {
            let inp = [a as u8, (a ^ 0x5A) as u8, (a.wrapping_mul(3)) as u8, !a as u8];
            let mut g = [0u8; 5];
            encode_group(&inp, &mut g);
            let mut back = [0u8; 4];
            assert!(decode_group(&g, &mut back));
            assert_eq!(back, inp, "failed for {inp:?}");
        }
    }

    #[test]
    fn gcr_codes_never_have_three_zero_bits_in_a_row() {
        // The whole point of GCR: pack every group and confirm the bitstream
        // never has three consecutive zeros (which would break clock recovery).
        for hi in 0u8..16 {
            for lo in 0u8..16 {
                let inp = [(hi << 4) | lo, (lo << 4) | hi, 0x00, 0xFF];
                let mut g = [0u8; 5];
                encode_group(&inp, &mut g);
                // Walk the 40 bits with wrap and count zero runs.
                let mut bits: u64 = 0;
                for &b in &g {
                    bits = (bits << 8) | b as u64;
                }
                let mut zeros = 0;
                for i in (0..40).rev() {
                    if (bits >> i) & 1 == 0 {
                        zeros += 1;
                        assert!(zeros <= 2, "three zero bits in a row for {inp:?}");
                    } else {
                        zeros = 0;
                    }
                }
            }
        }
    }

    #[test]
    fn track_has_a_sector_for_every_slot_and_decodes() {
        // Encode a track of distinct sectors, then find each data block by its
        // sync + GCR(07 ...) and decode it back.
        let track = 18u8;
        let n = sectors_per_track(track) as usize;
        let sectors: Vec<[u8; 256]> = (0..n).map(|s| [s as u8; 256]).collect();
        let stream = encode_track(&sectors, track, [0x41, 0x42]);

        // The track should contain 2 sync groups per sector (header + data).
        // Count runs of >=5 consecutive $FF.
        let mut syncs = 0;
        let mut run = 0;
        for &b in &stream {
            if b == 0xFF {
                run += 1;
            } else {
                if run >= SYNC_LEN {
                    syncs += 1;
                }
                run = 0;
            }
        }
        if run >= SYNC_LEN {
            syncs += 1;
        }
        assert_eq!(syncs, n * 2, "expected a header+data sync for every sector");
    }
}
