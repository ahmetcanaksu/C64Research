//! Diagnose a game that hangs, reproducing the real bus-load path.
//!   cargo run -p emu --example diag --release -- prg/Q-bert.d64 [runframes]

use harness::Harness;

fn main() {
    let path = std::env::args().nth(1).expect("usage: diag <file.d64|.prg> [frames]");
    let run_frames: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let bytes = std::fs::read(&path).expect("read file");

    let is_d64 = path.to_ascii_lowercase().ends_with(".d64");
    let mut h = if is_d64 {
        // The authentic path: a drive on the bus, LOAD over three wires, RUN.
        let disk = d64::Disk::new(bytes).expect("valid .d64");
        let mut h = Harness::with_disk(disk).expect("ROMs in roms/");
        h.boot();
        h.type_text("load\"*\",8,1\r");
        assert!(h.wait_for("LOADING", 5000), "drive never answered:\n{}", h.screen());
        // Wait for the load to finish (KERNAL sets ST bit 6 = EOI at $90).
        let done = h.run_until(200_000, |c| c.board.ram[0x90] & 0x40 != 0);
        println!("load finished: {done}  (ST=${:02X})", h.c64.board.ram[0x90]);
        h.type_text("run\r");
        h
    } else {
        let mut h = Harness::new().expect("ROMs in roms/");
        h.boot();
        h.c64.load_prg(&bytes);
        h.type_text("run\r");
        h
    };
    h.run_frames(run_frames);

    let c = &mut h.c64;
    let vr = c.board.vic.regs;
    println!("\n--- after RUN + {run_frames} frames ---");
    println!(
        "CPU  PC:{:04X}  I(irq-disabled):{}  cycles:{}",
        c.cpu.pc, c.cpu.i as u8, c.cpu.cycles
    );
    let raster_cmp = vr[0x12] as u16 | (((vr[0x11] & 0x80) as u16) << 1);
    println!(
        "VIC  D011:{:02X} D016:{:02X}  raster:{}  compare:{}  D01A(en):{:02X}  irq_asserted:{}",
        vr[0x11],
        vr[0x16],
        c.board.vic.raster(),
        raster_cmp,
        vr[0x1A],
        c.board.vic.irq_asserted()
    );
    println!("$0314 IRQ vector -> ${:02X}{:02X}", c.board.ram[0x0315], c.board.ram[0x0314]);
    println!("SPR enable D015:{:02X}", vr[0x15]);
    println!("screen row 0: {:?}", screen_text(&c.board.ram, 0));

    // Stuck? sample PC.
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let (mut lo, mut hi) = (0xFFFFu16, 0u16);
    let mut in_kernal_irq = 0u32;
    for _ in 0..200_000u32 {
        let pc = c.cpu.pc;
        seen.insert(pc);
        lo = lo.min(pc);
        hi = hi.max(pc);
        if (0xEA00..0xEB00).contains(&pc) {
            in_kernal_irq += 1;
        }
        c.step();
    }
    println!(
        "\nPC over 200k steps: {} distinct, range ${lo:04X}..${hi:04X}, {in_kernal_irq} in KERNAL-IRQ  {}",
        seen.len(),
        if seen.len() < 500 { "<< STUCK" } else { "(running)" }
    );

    // Does holding the '1' key (matrix 56) break it out?
    let before = c.cpu.pc;
    for _ in 0..180u32 {
        c.board.key_matrix = [0; 8];
        c.board.set_key(56, true); // '1'
        let deadline = c.cpu.cycles.wrapping_add(19_700);
        while c.cpu.cycles < deadline {
            c.step();
        }
    }
    println!(
        "held '1' key: PC {before:04X} -> {:04X}  {}",
        c.cpu.pc,
        if (lo..=hi).contains(&c.cpu.pc) { "(still in same region)" } else { "<< MOVED ON" }
    );
    println!("screen row 0 now: {:?}", screen_text(&c.board.ram, 0));
}

fn screen_text(ram: &[u8], row: usize) -> String {
    (0..40)
        .map(|col| {
            let sc = ram[0x0400 + row * 40 + col] & 0x7F;
            match sc {
                0 => '@',
                1..=26 => (b'A' + sc - 1) as char,
                0x20..=0x3F => sc as char,
                _ => '.',
            }
        })
        .collect()
}
