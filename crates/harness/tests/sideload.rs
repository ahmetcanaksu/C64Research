//! Side-loading a real program straight into RAM — no bus, no drive.
//!
//! This is the shortcut path (`C64::load_prg`), and it earns its keep as a test
//! because a real commercial game leans on the undocumented opcodes and on
//! VIC-II behaviour that synthetic fixtures never touch.
//!
//! Needs `prg/64quarx-shareware.prg`, which is not redistributable and so is not
//! committed. Drop one in and this starts running.

use harness::Harness;

#[test]
fn loads_and_runs_a_real_game() {
    if harness::skip_without_roms!("loads_and_runs_a_real_game") {
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../prg/64quarx-shareware.prg");
    let Ok(prg) = std::fs::read(path) else {
        eprintln!("SKIP loads_and_runs_a_real_game: prg/64quarx-shareware.prg not present");
        return;
    };

    let mut h = Harness::new().unwrap();
    h.boot();

    let addr = h.c64.load_prg(&prg);
    assert_eq!(addr, 0x0801, "expected a BASIC-startable program");
    h.type_text("run\r");

    // The game's own machine code should take over. BASIC and the KERNAL run
    // from ROM at $A000+, so a PC down in RAM means the SYS landed.
    let mut ram_pc_hits = 0u64;
    h.run_until_step(3000, |c| {
        if (0x0810..0xA000).contains(&c.cpu.pc) {
            ram_pc_hits += 1;
        }
        ram_pc_hits > 1000
    });
    assert!(ram_pc_hits > 1000, "the game's code never ran:\n{}", h.screen());

    // ...and it should have drawn something the VIC can render.
    let mut fb = vec![0u32; 320 * 200];
    for _ in 0..8 {
        h.c64.run_frame(&mut fb);
    }
    let colours: std::collections::HashSet<u32> = fb.iter().copied().collect();
    let cells = h.c64.board.ram[0x0400..0x07E8].iter().filter(|&&b| b != 0 && b != 0x20).count();
    assert!(cells > 50 && colours.len() >= 2, "no screen drawn ({cells} cells, {} colours)", colours.len());
    eprintln!("Quarx drew {cells} cells in {} colours", colours.len());
}
