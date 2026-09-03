//! Boot the real KERNAL + BASIC on the emulated C64 and confirm it reaches the
//! `READY.` prompt — the headline test for the live machine.
//!
//! Needs the (gitignored) C64 ROMs in `roms/`; skips if they're absent. Fetch:
//!   see roms/README.md

use std::path::PathBuf;

fn rom(name: &str) -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../roms")
        .join(name);
    std::fs::read(p).ok()
}

/// Screen code -> a printable ASCII char, for dumping the screen on failure.
fn sc_to_ascii(sc: u8) -> char {
    match sc & 0x7F {
        0 => '@',
        c @ 1..=26 => (b'A' + c - 1) as char,
        c @ 0x20..=0x3F => c as char,
        _ => '.',
    }
}

fn dump_screen(ram: &[u8]) -> String {
    let mut s = String::new();
    for row in 0..25 {
        for col in 0..40 {
            s.push(sc_to_ascii(ram[0x0400 + row * 40 + col]));
        }
        s.push('\n');
    }
    s
}

/// Find `needle` (screen codes) anywhere in the 1000-cell screen.
fn screen_contains(ram: &[u8], needle: &[u8]) -> bool {
    ram[0x0400..0x07E8]
        .windows(needle.len())
        .any(|w| w == needle)
}

#[test]
fn boots_to_ready_prompt() {
    let (Some(kernal), Some(basic), Some(chargen)) = (
        rom("kernal-901227-03.bin"),
        rom("basic-901226-01.bin"),
        rom("chargen-901225-01.bin"),
    ) else {
        eprintln!("skipping boots_to_ready_prompt: C64 ROMs not in roms/ (see roms/README.md)");
        return;
    };

    let mut c64 = Box::new(c64::C64::new(&kernal, &basic, &chargen));
    // Reset vector must land in the KERNAL at the canonical reset entry.
    assert_eq!(c64.cpu.pc, 0xFCE2, "reset vector should point at KERNAL $FCE2");

    // "READY" in C64 screen codes: R E A D Y.
    const READY: [u8; 5] = [0x12, 0x05, 0x01, 0x04, 0x19];

    let mut booted = false;
    // Boot (RAM test + BASIC memory test + printing) is a few million cycles.
    for i in 0..40_000_000u64 {
        c64.step();
        if i % 200_000 == 0 && screen_contains(&c64.board.ram, &READY) {
            booted = true;
            break;
        }
    }

    assert!(
        booted,
        "never reached READY.; PC=${:04X}\nscreen:\n{}",
        c64.cpu.pc,
        dump_screen(&c64.board.ram)
    );

    eprintln!("\n===== C64 boot screen =====\n{}", dump_screen(&c64.board.ram));

    // Sanity: the classic banner is on screen too, and the VIC colours are set.
    const COMMODORE: [u8; 9] = [0x03, 0x0F, 0x0D, 0x0D, 0x0F, 0x04, 0x0F, 0x12, 0x05]; // COMMODORE
    assert!(
        screen_contains(&c64.board.ram, &COMMODORE),
        "banner missing:\n{}",
        dump_screen(&c64.board.ram)
    );
    assert_eq!(c64.board.vic.regs[0x20], 0x0E, "border light blue");
    assert_eq!(c64.board.vic.regs[0x21], 0x06, "background blue");
}
