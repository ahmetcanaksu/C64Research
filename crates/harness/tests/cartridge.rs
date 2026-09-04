//! A cartridge taking over the machine at reset.
//!
//! This is the one boot path that never reaches BASIC. The KERNAL's reset checks
//! for a `CBM80` signature at `$8004` *before* it initialises anything, and if
//! it finds one it jumps through the cartridge's own cold-start vector and never
//! comes back. So the test for "did the cartridge work" is not what appears on
//! screen — it is that the cartridge's code ran and BASIC never started.

use harness::c64::Cartridge;
use harness::Harness;

/// A minimal autostart cartridge, hand-assembled.
///
/// The layout of the first nine bytes is fixed by the KERNAL:
///
/// ```text
///   $8000  cold-start vector (little-endian)
///   $8002  warm-start vector (NMI)
///   $8004  the five signature bytes "CBM80"
///   $8009  ...your code
/// ```
///
/// The reset routine has already set up the stack and cleared decimal mode by
/// the time it jumps here, but nothing else: no I/O is initialised, there is no
/// screen, and no interrupts. A real cartridge does all of that itself.
fn marker_cartridge() -> Vec<u8> {
    let mut rom = vec![0u8; 0x2000];
    rom[0x00] = 0x09; // cold start -> $8009
    rom[0x01] = 0x80;
    rom[0x02] = 0x09; // warm start -> the same
    rom[0x03] = 0x80;
    rom[0x04..0x09].copy_from_slice(&[0xC3, 0xC2, 0xCD, 0x38, 0x30]); // "CBM80"

    let code: &[u8] = &[
        0xA9, 0x42, // LDA #$42
        0x8D, 0x00, 0xC0, // STA $C000   ; a marker in plain RAM
        0xA9, 0x43, // LDA #$43
        0x8D, 0x01, 0xC0, // STA $C001
        0x4C, 0x13, 0x80, // JMP $8013   ; spin here forever
    ];
    rom[0x09..0x09 + code.len()].copy_from_slice(code);
    rom
}

#[test]
fn an_autostart_cartridge_runs_instead_of_basic() {
    if harness::skip_without_roms!("an_autostart_cartridge_runs_instead_of_basic") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.c64.insert_cartridge(Cartridge::lo8k(&marker_cartridge()));

    // Reset lands in the KERNAL as always — the cartridge is found from there.
    assert_eq!(h.c64.cpu.pc, 0xFCE2, "reset still enters the KERNAL");

    let ran = h.run_until_step(200, |c| {
        c.board.ram[0xC000] == 0x42 && c.board.ram[0xC001] == 0x43
    });
    assert!(ran, "the cartridge's code never ran (pc ${:04X})", h.c64.cpu.pc);

    // It spins at $8013, so the machine is *in* the cartridge, not past it.
    h.run_frames(20);
    assert_eq!(h.c64.cpu.pc, 0x8013, "should be looping inside the cartridge");

    // And BASIC never got a chance to start.
    assert!(
        !h.screen_contains("READY."),
        "BASIC started anyway — the cartridge did not take over:\n{}",
        h.screen()
    );
}

/// Without the signature the KERNAL ignores the cartridge and boots BASIC — but
/// the cartridge is still *there*, and the machine tells you so.
///
/// A stock C64 reports `38911 BASIC BYTES FREE`. With 8 KB of cartridge ROM at
/// `$8000`, RAMTAS's memory scan writes its test pattern, reads back ROM instead,
/// and concludes that RAM ends there — so BASIC gets 8192 bytes less and reports
/// **30719**. Nobody implemented that subtraction; it falls out of the same
/// write-hits-RAM/read-hits-ROM rule that finds the top of RAM in the first
/// place, which is a good sign the banking is modelled and not faked.
#[test]
fn a_cartridge_without_the_signature_is_ignored_but_still_costs_ram() {
    if harness::skip_without_roms!("a_cartridge_without_the_signature_is_ignored_but_still_costs_ram")
    {
        return;
    }
    let mut rom = marker_cartridge();
    rom[0x04] = 0x00; // break "CBM80"

    let mut h = Harness::new().unwrap();
    h.c64.insert_cartridge(Cartridge::lo8k(&rom));
    h.boot(); // reaches READY. as usual

    let s = h.screen();
    assert!(s.contains("COMMODORE 64 BASIC V2"), "BASIC should still boot:\n{s}");
    assert!(s.contains("30719 BASIC BYTES FREE"), "8K of cartridge should cost 8K of RAM:\n{s}");
    assert_eq!(h.c64.board.ram[0xC000], 0x00, "the cartridge code must not have run");
    // ...and its ROM is still mapped, just never entered.
    assert_eq!(h.c64.board.peek(0x8009), 0xA9, "ROML is still visible at $8009");
    // MEMSIZ ($0283/$0284) is where RAMTAS recorded the top of RAM.
    assert_eq!(h.c64.board.ram[0x0284], 0x80, "top of RAM is $8000, not $A000");
}

/// A 16 KB cartridge displaces BASIC, so RAMTAS finds a different top of RAM and
/// the machine can never print the familiar `38911 BASIC BYTES FREE`.
#[test]
fn a_16k_cartridge_displaces_basic() {
    if harness::skip_without_roms!("a_16k_cartridge_displaces_basic") {
        return;
    }
    let mut rom = marker_cartridge();
    rom.resize(0x4000, 0);
    rom[0x2000] = 0xEE; // something recognisable in ROMH

    let mut h = Harness::new().unwrap();
    h.c64.insert_cartridge(Cartridge::hi16k(&rom));
    let ran = h.run_until_step(200, |c| c.board.ram[0xC000] == 0x42);
    assert!(ran, "the cartridge's code never ran");
    assert_eq!(h.c64.board.peek(0xA000), 0xEE, "ROMH replaces BASIC at $A000");
}
