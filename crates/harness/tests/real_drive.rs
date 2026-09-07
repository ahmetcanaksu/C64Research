//! `LOAD` served by the **real 1541 firmware** — no host-side shortcut anywhere.
//!
//! Everything else in this suite talks to `iec::Device`, which simulates the
//! serial protocol from the host side. Here that is unplugged and a whole
//! emulated 1541 answers instead: its own 6502 running Commodore's DOS ROM, two
//! VIAs, a stepper, and a disk turning under the head with the data GCR-encoded
//! the way a real disk stores it.
//!
//! So a passing test here means two 6502s handshook over three wires, the DOS
//! bumped and seeked its head to the right track, found a sync mark, decoded GCR
//! back into bytes, and shipped them up the bus — with nothing in the path
//! pretending on anyone's behalf.

use harness::{d64, Harness};

fn disk() -> d64::Disk {
    d64::Disk::new(d64::fixtures::synthetic_image()).expect("valid fixture")
}

fn program_disk() -> d64::Disk {
    d64::Disk::new(d64::fixtures::basic_program_image()).expect("valid fixture")
}

/// Both machines switched on together: the C64 reaches `READY.` and the drive
/// reaches its DOS idle loop, each on its own clock.
#[test]
fn both_machines_boot_side_by_side() {
    if harness::skip_without_roms!("both_machines_boot_side_by_side") {
        return;
    }
    let mut h = Harness::with_real_drive(disk()).unwrap();
    h.boot();

    assert!(h.screen_contains("READY."), "the C64 should be at the prompt:\n{}", h.screen());
    // The drive's idle loop head, as `c1541`'s own boot test uses.
    let pc = h.real_drive.as_ref().unwrap().machine.cpu.pc;
    assert!(
        (0xE000..=0xEFFF).contains(&pc),
        "the drive should be idling in its DOS, not stuck at ${pc:04X}"
    );
}

/// `LOAD"$",8` — the directory, read off a GCR disk by the real DOS.
#[test]
fn loads_the_directory_from_the_real_drive() {
    if harness::skip_without_roms!("loads_the_directory_from_the_real_drive") {
        return;
    }
    let mut h = Harness::with_real_drive(disk()).unwrap();
    h.boot();
    h.type_text("load\"$\",8\r");

    assert!(h.wait_for("SEARCHING FOR $", 600), "load never started:\n{}", h.screen());
    assert!(
        h.wait_for("LOADING", 4000),
        "the real drive never answered (head on track {}):\n{}",
        h.real_drive.as_ref().unwrap().track(),
        h.screen()
    );

    // Done when the KERNAL has seen EOI and BASIC's program pointer moved.
    assert!(
        h.run_until(8000, |c| c.board.ram[0x90] & 0x40 != 0
            && u16::from_le_bytes([c.board.ram[0x2D], c.board.ram[0x2E]]) > 0x0801),
        "the load never completed (STATUS ${:02X}):\n{}",
        h.io_status(),
        h.screen()
    );

    h.type_text("list\r");
    assert!(h.wait_for("BLOCKS FREE.", 8000), "LIST did not finish:\n{}", h.screen());

    let s = h.screen();
    assert!(s.contains("HELLO"), "the entry is missing:\n{s}");
    assert!(s.contains("PRG"), "the type is missing:\n{s}");
    assert!(s.contains("TEST DISK"), "the disk name is missing:\n{s}");
}

/// The whole stack, for real: load a BASIC program off a GCR disk through the
/// genuine firmware, `RUN` it, and read its output.
#[test]
fn loads_and_runs_a_program_from_the_real_drive() {
    if harness::skip_without_roms!("loads_and_runs_a_program_from_the_real_drive") {
        return;
    }
    let mut h = Harness::with_real_drive(program_disk()).unwrap();
    h.boot();
    h.type_text("load\"*\",8,1\r");

    assert!(h.wait_for("SEARCHING FOR *", 600), "load never started:\n{}", h.screen());
    // Wait for the transfer to actually finish before typing anything else.
    // A serial transfer runs with interrupts disabled, so the KERNAL's keyboard
    // scan is not running — anything typed into it now is simply lost.
    assert!(
        h.run_until(12000, |c| c.board.ram[0x90] & 0x40 != 0),
        "the load never completed (head on track {}, STATUS ${:02X}):\n{}",
        h.real_drive.as_ref().unwrap().track(),
        h.io_status(),
        h.screen()
    );
    assert!(h.wait_until_typed(2000), "editor never drained:\n{}", h.screen());

    h.type_text("run\r");
    assert!(
        h.wait_for("HELLO FROM DISK", 4000),
        "the program never ran:\n{}",
        h.screen()
    );
    assert!(!h.screen_contains("ERROR"), "BASIC reported an error:\n{}", h.screen());
}

/// The head really does move: the directory lives on track 18, and a drive that
/// has just powered on has its head at the rim.
#[test]
fn the_drive_seeks_to_the_directory_track() {
    if harness::skip_without_roms!("the_drive_seeks_to_the_directory_track") {
        return;
    }
    let mut h = Harness::with_real_drive(disk()).unwrap();
    h.boot();
    assert_eq!(h.real_drive.as_ref().unwrap().track(), 1, "parked at the rim after boot");

    h.type_text("load\"$\",8\r");
    assert!(h.wait_for("LOADING", 4000), "no answer from the drive:\n{}", h.screen());

    assert_eq!(
        h.real_drive.as_ref().unwrap().track(),
        18,
        "the DOS should have seeked to the directory track"
    );
}
