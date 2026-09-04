//! Reading the C64's text screen.
//!
//! The screen is 1000 bytes of **screen codes** at `$0400` — not PETSCII and not
//! ASCII. `A` is 1, not 65; `@` is 0. Bit 7 is the reverse-video flag rather
//! than part of the character, which is the detail that catches people out: a
//! directory header is printed in reverse, so every code in it has bit 7 set and
//! a naive byte comparison finds nothing.

/// Where screen memory lives by default, and how big it is.
pub const SCREEN: usize = 0x0400;
pub const COLS: usize = 40;
pub const ROWS: usize = 25;
pub const CELLS: usize = COLS * ROWS;

/// One screen code as a printable char, for dumps and failure messages.
///
/// Lossy on purpose: graphics characters become `.` so a dump stays readable in
/// a terminal. Don't parse the result — use [`contains`] to look for text.
pub fn code_to_char(code: u8) -> char {
    match code & 0x7F {
        0 => '@',
        c @ 1..=26 => (b'A' + c - 1) as char,
        c @ 0x20..=0x3F => c as char,
        _ => '.',
    }
}

/// ASCII text as the screen codes the editor would store for it.
pub fn codes_for(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' => b - b'A' + 1,
            b'a'..=b'z' => b - b'a' + 1,
            b'@' => 0,
            other => other, // space, digits and punctuation coincide
        })
        .collect()
}

/// The whole screen as text, one line per row, trailing blanks trimmed.
pub fn text(ram: &[u8]) -> String {
    (0..ROWS)
        .map(|row| {
            (0..COLS)
                .map(|col| code_to_char(ram[SCREEN + row * COLS + col]))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does the screen show `text` anywhere?
///
/// Compares with bit 7 masked off, so reverse-video text matches too, and it
/// looks across the whole 1000 cells rather than line by line — screen memory
/// is contiguous, so a string that wraps at column 40 still matches.
pub fn contains(ram: &[u8], text: &str) -> bool {
    let needle = codes_for(text);
    if needle.is_empty() || needle.len() > CELLS {
        return false;
    }
    ram[SCREEN..SCREEN + CELLS]
        .windows(needle.len())
        .any(|w| w.iter().zip(&needle).all(|(a, b)| a & 0x7F == *b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> Vec<u8> {
        let mut ram = vec![0u8; 0x10000];
        ram[SCREEN..SCREEN + CELLS].fill(0x20);
        ram
    }

    fn write(ram: &mut [u8], row: usize, s: &str) {
        for (i, c) in codes_for(s).into_iter().enumerate() {
            ram[SCREEN + row * COLS + i] = c;
        }
    }

    #[test]
    fn finds_plain_text() {
        let mut ram = blank();
        write(&mut ram, 3, "READY.");
        assert!(contains(&ram, "READY."));
        assert!(!contains(&ram, "NOTHING"));
    }

    /// The one that matters: reverse video must not hide text.
    #[test]
    fn finds_reverse_video_text() {
        let mut ram = blank();
        write(&mut ram, 0, "TEST DISK");
        for cell in &mut ram[SCREEN..SCREEN + COLS] {
            *cell |= 0x80; // RVS ON, as a directory header is printed
        }
        assert!(contains(&ram, "TEST DISK"), "bit 7 is reverse video, not the character");
    }

    #[test]
    fn dump_is_readable_and_trimmed() {
        let mut ram = blank();
        write(&mut ram, 0, "HELLO");
        let dump = text(&ram);
        assert_eq!(dump.lines().next(), Some("HELLO"), "trailing blanks trimmed");
        // `split` rather than `lines`: every row is present, including blank
        // trailing ones, which `lines()` would swallow after the final newline.
        assert_eq!(dump.split('\n').count(), ROWS, "one line per screen row");
    }

    #[test]
    fn an_empty_needle_matches_nothing() {
        assert!(!contains(&blank(), ""));
    }
}
