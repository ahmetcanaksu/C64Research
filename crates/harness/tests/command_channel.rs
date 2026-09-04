//! The drive's command/error channel (secondary address 15), through real BASIC.
//!
//! This is how a C64 program asks a drive what went wrong. It is worth testing
//! from BASIC rather than against the trait, because the interesting part is the
//! *shape* of the reply: the drive must answer `code,MESSAGE,track,sector` so
//! that `INPUT#15,A,B$` splits it the way every C64 program expects.
//!
//! `INPUT#` is illegal in direct mode, so these type a small program and RUN it.

use harness::{d64, Harness};

fn disk() -> d64::Disk {
    d64::Disk::new(d64::fixtures::synthetic_image()).expect("valid fixture")
}

/// Type a numbered program and run it.
fn run_program(h: &mut Harness, lines: &[&str]) {
    for line in lines {
        h.type_text(line);
        h.type_text("\r");
    }
    // Let the editor take every line before RUN.
    assert!(h.wait_until_typed(3000), "program never finished typing:\n{}", h.screen());
    h.type_text("run\r");
}

/// A freshly powered drive reports its DOS banner — status 73, not 00.
#[test]
fn reads_the_power_on_status() {
    if harness::skip_without_roms!("reads_the_power_on_status") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();
    run_program(
        &mut h,
        &["10 open15,8,15", "20 input#15,a,b$", "30 print a;b$", "40 close15"],
    );

    assert!(
        h.wait_for("73 CBM DOS V2.6 1541", 4000),
        "the drive did not report its DOS version:\n{}",
        h.screen()
    );
}

/// The one that makes failures debuggable: a missing file leaves `62,FILE NOT
/// FOUND` on the error channel, so a program can find out *why* a load failed.
#[test]
fn reports_file_not_found_after_a_failed_load() {
    if harness::skip_without_roms!("reports_file_not_found_after_a_failed_load") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();

    // Fail a load first, then ask the drive about it.
    h.type_text("load\"nope\",8,1\r");
    assert!(
        h.wait_for("FILE NOT FOUND", 3000),
        "BASIC should report the failure itself:\n{}",
        h.screen()
    );
    assert!(h.wait_until_typed(2000));

    run_program(
        &mut h,
        &["10 open15,8,15", "20 input#15,a,b$", "30 print a;b$", "40 close15"],
    );
    assert!(
        h.wait_for("62 FILE NOT FOUND", 4000),
        "the error channel did not explain the failure:\n{}",
        h.screen()
    );
}

/// A write command on a read-only drive is refused out loud, not ignored.
#[test]
fn refuses_a_write_command_with_write_protect_on() {
    if harness::skip_without_roms!("refuses_a_write_command_with_write_protect_on") {
        return;
    }
    let mut h = Harness::with_disk(disk()).unwrap();
    h.boot();
    run_program(
        &mut h,
        &[
            "10 open15,8,15",
            "20 print#15,\"s:hello\"",
            "30 input#15,a,b$",
            "40 print a;b$",
            "50 close15",
        ],
    );

    assert!(
        h.wait_for("26 WRITE PROTECT ON", 4000),
        "scratching a file should be refused:\n{}",
        h.screen()
    );
}
