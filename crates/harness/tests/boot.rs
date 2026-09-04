//! Booting the real machine, and what the serial bus looks like when it lands.

use harness::{iec, screen, Harness};

#[test]
fn boots_to_ready_prompt() {
    if harness::skip_without_roms!("boots_to_ready_prompt") {
        return;
    }
    let mut h = Harness::new().unwrap();

    // The reset vector must point at the KERNAL's documented reset entry.
    assert_eq!(h.c64.cpu.pc, 0xFCE2, "reset vector should point at KERNAL $FCE2");

    h.boot();
    let s = h.screen();
    assert!(s.contains("COMMODORE 64 BASIC V2"), "no BASIC banner:\n{s}");
    assert!(s.contains("38911 BASIC BYTES FREE"), "wrong free-memory figure:\n{s}");
}

/// The idle bus after boot: **ATN and DATA released, CLK pulled low**.
///
/// IOINIT writes `$07` to `$DD00`, releasing all three lines — but its very last
/// instruction is `JMP $EE8E`, and `$EE8E` is CLKLO (`ORA #$10`), which pulls the
/// clock line to ground. Being a `JMP` tail-call rather than a `JSR`, it is easy
/// to read straight past when tracing the boot.
///
/// So a C64 at the `READY.` prompt holds CLK low. Nothing minds: a drive waits on
/// ATN, and the KERNAL's own send routines pull CLK low before sending anyway.
#[test]
fn idle_bus_holds_clk_low_after_boot() {
    if harness::skip_without_roms!("idle_bus_holds_clk_low_after_boot") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.boot();

    let state = format!(
        "$DD00 level ${:02X}, pulls ${:02X}",
        h.c64.board.cia2.port_a(),
        h.c64.board.iec.pulls_of(iec::CONTROLLER)
    );
    assert!(h.c64.board.iec.atn(), "ATN released when idle ({state})");
    assert!(h.c64.board.iec.data(), "DATA released when idle ({state})");
    assert!(
        h.c64.board.iec.pulled_low(iec::Line::Clk),
        "IOINIT's closing CLKLO should leave CLK low ({state})"
    );
}

/// Typing works at all: the editor echoes what it is sent, and BASIC evaluates it.
#[test]
fn types_at_the_basic_prompt() {
    if harness::skip_without_roms!("types_at_the_basic_prompt") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.boot();
    h.type_text("print 1+1\r");

    assert!(h.wait_for("PRINT 1+1", 200), "the line never echoed:\n{}", h.screen());
    // BASIC prints a leading space for a non-negative number.
    assert!(h.wait_for(" 2", 200), "BASIC never printed the answer:\n{}", h.screen());
    assert!(h.dropped_keys().is_empty(), "dropped keystrokes: {:?}", h.dropped_keys());
}

/// A blank screen really is blank, so `screen_contains` can be trusted to mean
/// something when it says no.
#[test]
fn screen_helpers_agree_with_the_dump() {
    if harness::skip_without_roms!("screen_helpers_agree_with_the_dump") {
        return;
    }
    let mut h = Harness::new().unwrap();
    h.boot();
    assert!(h.screen_contains("READY."));
    assert!(!h.screen_contains("SEARCHING FOR"));
    assert_eq!(screen::text(&h.c64.board.ram), h.screen());
}
