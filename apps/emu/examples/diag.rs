//! Trace Q-bert's runtime second load: side-load file 1 (fast) but keep a drive
//! on the bus, reach the player-select, pick 1 player, and watch whether the
//! game loads its second file and fills $C800.
//!   cargo run -p emu --example diag --release -- prg/Q-bert.d64

use harness::Harness;

fn main() {
    let path = std::env::args().nth(1).expect("usage: diag <file.d64>");
    let raw = std::fs::read(&path).expect("read file");

    let file1 = d64::Disk::new(raw.clone()).unwrap().read_prg("*").expect("first PRG");
    let mut h = Harness::with_disk(d64::Disk::new(raw).unwrap()).expect("ROMs in roms/");
    h.boot();
    h.c64.load_prg(&file1);
    // Poison the table region so we can tell "written with zeros" from "never
    // touched at all".
    for a in 0xC800..0xC900 {
        h.c64.board.ram[a] = 0xAA;
    }
    println!("side-loaded file 1 ({} bytes); running...", file1.len());
    h.type_text("run\r");

    // Reach the player-select read routine ($9100-$9130).
    let at_menu = h.run_until_step(5_000_000, |c| (0x9100..0x9130).contains(&c.cpu.pc));
    println!("reached player-select: {at_menu} (PC ${:04X})", h.c64.cpu.pc);

    // Hold '1' (matrix 56) and let it run, watching for the second load + $C800.
    let mut filled_at = None;
    for frame in 0..4000u32 {
        h.c64.board.key_matrix = [0; 8];
        h.c64.board.set_key(56, true); // '1'
        let deadline = h.c64.cpu.cycles.wrapping_add(19_700);
        while h.c64.cpu.cycles < deadline {
            let cyc = h.c64.step();
            if let Some(d) = h.drive.as_mut() {
                d.tick(&mut h.c64.board.iec, cyc as u32);
            }
        }
        if filled_at.is_none() && h.c64.board.ram[0xC800] != 0 {
            filled_at = Some(frame);
        }
    }

    let (name, ok) = h.drive.as_ref().unwrap().disk.last_open();
    println!("\n--- result ---");
    println!("drive last open : '{name}'  ok={ok}");
    println!("$C800 filled    : {:?}", filled_at);
    println!("$C800 bytes     : {:02X?}", &h.c64.board.ram[0xC800..0xC810]);
    println!("CPU PC          : ${:04X}", h.c64.cpu.pc);
    println!("screen row 2    : {:?}", row(&h.c64.board.ram, 2));
    println!("screen row 3    : {:?}", row(&h.c64.board.ram, 3));
}

fn row(ram: &[u8], r: usize) -> String {
    (0..40)
        .map(|c| {
            let sc = ram[0x0400 + r * 40 + c] & 0x7F;
            match sc {
                0 => '@',
                1..=26 => (b'A' + sc - 1) as char,
                0x20..=0x3F => sc as char,
                _ => '.',
            }
        })
        .collect()
}
