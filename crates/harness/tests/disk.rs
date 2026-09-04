//! Loading from a disk over the emulated serial bus, driven by the real KERNAL.
//!
//! These are the seam tests: the KERNAL asserts ATN and addresses device 8, a
//! filename crosses three wires a bit at a time, the bus turns around, and a
//! file comes back. Every one of them goes through the genuine ROM, so a
//! polarity error or a handshake that stalls shows up here and nowhere else.

use harness::{d64, iec, Harness};

fn disk() -> d64::Disk {
    d64::Disk::new(d64::fixtures::synthetic_image()).expect("valid fixture")
}

fn program_disk() -> d64::Disk {
    d64::Disk::new(d64::fixtures::basic_program_image()).expect("valid fixture")
}

/// The headline polarity test: `LOAD"$",8` with **no drive** must report
/// `?DEVICE NOT PRESENT ERROR`.
///
/// This exercises both directions at once. If the CIA2 *output* bits were not
/// inverted the KERNAL would never actually assert ATN and CLK; if the *input*
/// bits were inverted, an empty bus would look like a device holding DATA low
/// and the KERNAL would hang waiting for a byte instead of reporting an error.
/// Only correct polarity both ways produces this message.
#[test]
fn load_with_no_drive_reports_device_not_present() {
    if harness::skip_without_roms!("load_with_no_drive_reports_device_not_present") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.boot();
    h.type_text("load\"$\",8\r");

    // Per *step*, not per frame: the ATN pulse is microseconds long.
    let mut saw_atn = false;
    let found = h.run_until_step(2000, |c| {
        saw_atn |= !c.board.iec.atn();
        saw_atn && harness::screen::contains(&c.board.ram, "DEVICE NOT PRESENT")
    });

    assert!(saw_atn, "the KERNAL never pulled ATN low:\n{}", h.screen());
    assert!(found, "no DEVICE NOT PRESENT — are the input bits inverted?\n{}", h.screen());
    assert_eq!(h.io_status() & 0x80, 0x80, "STATUS bit 7 = device not present");
}

/// A stand-in device that only holds DATA low is enough to convince the KERNAL
/// something is out there — and it can never see DATA float while that is true,
/// which is the wired-OR property read back through the CIA.
#[test]
fn a_device_holding_data_low_is_seen_as_present() {
    if harness::skip_without_roms!("a_device_holding_data_low_is_seen_as_present") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.boot();
    h.c64.board.iec.pull(1, iec::Line::Data, true);
    h.type_text("load\"$\",8\r");

    let mut data_floated = false;
    let errored = h.run_until_step(400, |c| {
        data_floated |= c.board.iec.data();
        harness::screen::contains(&c.board.ram, "DEVICE NOT PRESENT")
    });

    assert!(!data_floated, "DATA cannot float high while a device pulls it low");
    assert!(!errored, "reported DEVICE NOT PRESENT though a device answered:\n{}", h.screen());
}

/// `LOAD"$",8` then `LIST` — the directory, which a drive fabricates as a BASIC
/// program so that BASIC itself can render it.
#[test]
fn loads_and_lists_the_directory() {
    if harness::skip_without_roms!("loads_and_lists_the_directory") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();
    h.type_text("load\"$\",8\r");

    assert!(h.wait_for("SEARCHING FOR $", 400), "load never started:\n{}", h.screen());
    assert!(h.wait_for("LOADING", 400), "the drive never answered:\n{}", h.screen());

    // Done when the KERNAL has seen EOI and BASIC's end-of-program pointer moved.
    assert!(
        h.run_until(2000, |c| c.board.ram[0x90] & 0x40 != 0
            && u16::from_le_bytes([c.board.ram[0x2D], c.board.ram[0x2E]]) > 0x0801),
        "the load never completed (STATUS ${:02X}):\n{}",
        h.io_status(),
        h.screen()
    );
    assert_eq!(h.drive.as_ref().unwrap().disk.last_open(), ("$", true));

    h.type_text("list\r");
    assert!(h.wait_for("BLOCKS FREE.", 2000), "LIST did not finish:\n{}", h.screen());

    let s = h.screen();
    assert!(s.contains("HELLO"), "the entry is missing:\n{s}");
    assert!(s.contains("PRG"), "the type is missing:\n{s}");
    // Disk name and ID are printed *outside* the quotes, where LIST detokenises
    // — `$A0` there would render as `CLOSE`. Seeing them plainly proves those
    // fields are space-padded, not shifted-space padded.
    assert!(s.contains("TEST DISK"), "the disk name is missing:\n{s}");
    assert!(s.contains("01 2A"), "the disk ID / DOS type are missing:\n{s}");
    assert!(s.contains("663 BLOCKS FREE."), "wrong free count:\n{s}");
}

/// `LOAD"*",8,1` — a program file, loaded absolutely to its own load address.
#[test]
fn loads_a_program_to_its_load_address() {
    if harness::skip_without_roms!("loads_a_program_to_its_load_address") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();
    h.type_text("load\"*\",8,1\r");

    assert!(h.wait_for("SEARCHING FOR *", 400), "load never started:\n{}", h.screen());
    // The fixture is $0801: $AA $BB $CC. Watch for it arriving — once BASIC is
    // back at READY it relinks $0801/$0802 and the evidence is overwritten.
    assert!(
        h.run_until(2000, |c| c.board.ram[0x0801..0x0804] == [0xAA, 0xBB, 0xCC]),
        "the file's bytes never reached $0801 (STATUS ${:02X}):\n{}",
        h.io_status(),
        h.screen()
    );
    assert_eq!(h.drive.as_ref().unwrap().disk.last_open(), ("*", true));
    assert_eq!(h.io_status() & 0x40, 0x40, "the KERNAL should have seen EOI");
    assert!(!h.screen_contains("PRESS PLAY ON TAPE"), "went to the tape:\n{}", h.screen());
}

/// The full stack: load a BASIC program off disk, `RUN` it, read its output.
///
/// Keyboard translation, KERNAL serial routines, the wired-OR bus, the drive's
/// protocol and file handling, BASIC's relocating load, and the interpreter —
/// all of it has to be right for this one line to appear.
#[test]
fn loads_and_runs_a_basic_program() {
    if harness::skip_without_roms!("loads_and_runs_a_basic_program") {
        return;
    }
    let mut h = Harness::with_disk(program_disk()).unwrap();
    h.boot();
    h.type_text("load\"*\",8,1\rrun\r");

    assert!(h.wait_for("SEARCHING FOR *", 400), "load never started:\n{}", h.screen());
    assert!(
        h.wait_for("HELLO FROM DISK", 3000),
        "the program never ran:\n{}",
        h.screen()
    );
    let s = h.screen();
    assert!(!s.contains("ERROR"), "BASIC reported an error:\n{s}");
    assert!(h.dropped_keys().is_empty(), "dropped keystrokes: {:?}", h.dropped_keys());
}

/// Asking for a file that isn't there must not look like success.
#[test]
fn a_missing_file_does_not_load() {
    if harness::skip_without_roms!("a_missing_file_does_not_load") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();
    h.type_text("load\"nope\",8,1\r");

    assert!(h.wait_for("SEARCHING FOR NOPE", 400), "load never started:\n{}", h.screen());
    h.run_frames(600);

    let (name, ok) = h.drive.as_ref().unwrap().disk.last_open();
    assert_eq!((name, ok), ("NOPE", false), "the drive should have failed to open it");
    assert!(!h.screen_contains("READY.\nREADY."), "should not report a clean load");
}
