//! List the directory of a .d64 disk image.
//!   cargo run -p emu --example dir -- prg/Q-bert.d64

fn main() {
    let path = std::env::args().nth(1).expect("usage: dir <file.d64>");
    let disk = d64::Disk::new(std::fs::read(&path).expect("read")).expect("valid .d64");
    let entries = disk.dir();
    println!("{} file(s) on {path}:", entries.len());
    for e in &entries {
        let ty = match e.file_type & 0x0F {
            1 => "SEQ",
            2 => "PRG",
            3 => "USR",
            4 => "REL",
            _ => "?",
        };
        let prg = disk.read_prg(&e.name);
        let load = prg
            .as_ref()
            .filter(|p| p.len() >= 2)
            .map(|p| format!("load ${:02X}{:02X}, {} bytes", p[1], p[0], p.len()))
            .unwrap_or_default();
        println!("  {:16}  {ty}  {:3} sectors  {load}", e.name, e.size_sectors);
    }
}
