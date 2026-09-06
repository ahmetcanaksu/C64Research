//! Load a program and dump the machine's state, to diagnose a game that hangs.
//!   cargo run -p emu --example diag -- prg/Q-bert.d64 [frames]

use harness::Harness;

fn main() {
    let path = std::env::args().nth(1).expect("usage: diag <file.prg|.d64> [frames]");
    let frames: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1500);

    let bytes = std::fs::read(&path).expect("read file");
    let prg = if path.to_ascii_lowercase().ends_with(".d64") {
        d64::Disk::new(bytes).expect("d64").read_prg("*").expect("a PRG on the disk")
    } else {
        bytes
    };

    let mut h = Harness::new().expect("ROMs in roms/");
    h.boot();
    let addr = h.c64.load_prg(&prg);
    println!("loaded {} -> ${addr:04X} ({} bytes)", path, prg.len());
    h.type_text("run\r");
    h.run_frames(frames);

    let c = &h.c64;
    let vr = &c.board.vic.regs;
    let cia2 = c.board.cia2_pra();
    let bank = (3 - (cia2 & 3) as usize) * 0x4000;
    let video = bank + ((vr[0x18] >> 4) as usize & 0x0F) * 0x0400;

    println!("\n--- after {frames} frames ---");
    println!("CPU  PC:{:04X}  cycles:{}", c.cpu.pc, c.cpu.cycles);
    println!(
        "VIC  D011:{:02X} D016:{:02X} D018:{:02X}  border:{:X} bg:{:X}",
        vr[0x11], vr[0x16], vr[0x18], vr[0x20] & 15, vr[0x21] & 15
    );
    println!(
        "SPR  enable:{:02X}  x-exp:{:02X} y-exp:{:02X} mc:{:02X} pri:{:02X}  mc0:{:X} mc1:{:X}",
        vr[0x15], vr[0x1D], vr[0x17], vr[0x1C], vr[0x1B], vr[0x25] & 15, vr[0x26] & 15
    );
    for i in 0..8 {
        if vr[0x15] & (1 << i) != 0 {
            let ptr = c.board.ram[(video + 0x3F8 + i) & 0xFFFF];
            let x = vr[i * 2] as u16 | (((vr[0x10] >> i) & 1) as u16) << 8;
            println!(
                "  spr{i}: x={:3} y={:3} col:{:X} ptr:${:02X} (data ${:04X})",
                x, vr[1 + i * 2], vr[0x27 + i] & 15, ptr, bank + ptr as usize * 64
            );
        }
    }
    println!("raster-IRQ enable D01A:{:02X}", vr[0x1A]);

    // Is the CPU stuck? Sample the PC over many raw steps (no drive/input).
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let (mut lo, mut hi) = (0xFFFFu16, 0u16);
    for _ in 0..100_000u32 {
        let pc = h.c64.cpu.pc;
        seen.insert(pc);
        lo = lo.min(pc);
        hi = hi.max(pc);
        h.c64.step();
    }
    println!(
        "\nPC over 100k steps: {} distinct, range ${lo:04X}..${hi:04X}  {}",
        seen.len(),
        if seen.len() < 400 { "<< STUCK IN A LOOP" } else { "(running)" }
    );

    // Dump the code region around the stuck loop so we can disassemble what it
    // polls. Read through the banking (peek) so we see what the CPU sees.
    let start = (lo as usize) & 0xFFF0;
    let end = ((hi as usize) + 0x20).min(0x1_0000);
    let mut region = Vec::new();
    for a in start..end {
        region.push(h.c64.board.ram[a]);
    }
    std::fs::write("stuck.bin", &region).unwrap();
    println!("wrote stuck.bin: ${start:04X}..${end:04X}  (disasm with --org {start:#06X})");

    // Hypothesis: it's waiting for the '2' key (matrix code 59). Hold it down and
    // see whether the loop breaks — proving the game just wants input.
    let before = h.c64.cpu.pc;
    for _ in 0..120u32 {
        h.c64.board.key_matrix = [0; 8];
        h.c64.board.set_key(59, true); // '2'
        let deadline = h.c64.cpu.cycles.wrapping_add(19_700);
        while h.c64.cpu.cycles < deadline {
            h.c64.step();
        }
    }
    let after = h.c64.cpu.pc;
    let escaped = !(0x9104..=0x93CC).contains(&after);
    println!(
        "\ninjected '2' key: PC {before:04X} -> {after:04X}  {}",
        if escaped { "<< LEFT THE LOOP: it was waiting for input!" } else { "(still looping)" }
    );
}
