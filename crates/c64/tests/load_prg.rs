//! Load a real `.prg` game, RUN it, and confirm its machine code takes over.
//!
//! Uses `prg/64quarx-shareware.prg` if present (and the C64 ROMs); skips
//! otherwise. This exercises the program loader *and* the illegal opcodes real
//! games depend on.

use std::path::PathBuf;

fn file(rel: &str) -> Option<Vec<u8>> {
    std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)).ok()
}

#[test]
fn loads_and_runs_quarx() {
    let (Some(kernal), Some(basic), Some(chargen)) = (
        file("../../roms/kernal-901227-03.bin"),
        file("../../roms/basic-901226-01.bin"),
        file("../../roms/chargen-901225-01.bin"),
    ) else {
        eprintln!("skipping loads_and_runs_quarx: C64 ROMs missing");
        return;
    };
    let Some(prg) = file("../../prg/64quarx-shareware.prg") else {
        eprintln!("skipping loads_and_runs_quarx: prg/64quarx-shareware.prg missing");
        return;
    };

    let mut c64 = Box::new(c64::C64::new(&kernal, &basic, &chargen));

    // Boot to READY.
    const READY: [u8; 5] = [0x12, 0x05, 0x01, 0x04, 0x19];
    let mut booted = false;
    for i in 0..40_000_000u64 {
        c64.step();
        if i % 100_000 == 0 && c64.board.ram[0x0400..0x07E8].windows(5).any(|w| w == READY) {
            booted = true;
            break;
        }
    }
    assert!(booted, "did not reach READY.");

    // Inject the program and RUN it via the keyboard buffer.
    let addr = c64.load_prg(&prg);
    assert_eq!(addr, 0x0801, "expected a BASIC-startable program");
    // "RUN" + RETURN into the KERNAL keyboard buffer ($0277), count in $C6.
    c64.board.ram[0x0277..0x027B].copy_from_slice(&[0x52, 0x55, 0x4E, 0x0D]);
    c64.board.ram[0x00C6] = 4;

    // Run a few million cycles and watch for the game's machine code executing
    // in RAM (below the BASIC ROM). BASIC/KERNAL run from ROM ($A000+), so PC
    // landing in $0810..$9FFF means the SYS jumped into the game.
    let mut ram_code_hits = 0u64;
    for _ in 0..4_000_000u64 {
        c64.step();
        if (0x0810..0xA000).contains(&c64.cpu.pc) {
            ram_code_hits += 1;
        }
    }

    assert!(
        ram_code_hits > 1000,
        "game machine code never took over (RAM-PC hits={ram_code_hits}); \
         RUN may not have executed or the CPU jammed"
    );

    // Render several frames and confirm the game actually produced a screen the
    // VIC renders (its title screen: text over a background).
    let mut fb = vec![0u32; 320 * 200];
    for _ in 0..8 {
        c64.run_frame(&mut fb);
    }
    let colors: std::collections::HashSet<u32> = fb.iter().copied().collect();
    let screen_nonzero = c64.board.ram[0x0400..0x07E8]
        .iter()
        .filter(|&&b| b != 0 && b != 0x20)
        .count();
    assert!(
        screen_nonzero > 50 && colors.len() >= 2,
        "game did not draw a screen (non-blank cells={screen_nonzero}, colours={})",
        colors.len()
    );
    eprintln!("Quarx drew {screen_nonzero} cells; frame has {} colours", colors.len());
}
