//! Headless screenshot tool: boot the C64, optionally load a program, render a
//! frame, and write it as a PPM image (openable in most image viewers/editors).
//!
//!   cargo run -p emu --example screenshot -- [file.prg|.d64] [frames] [out.ppm]
//!
//! e.g.  cargo run -p emu --example screenshot -- prg/64quarx-shareware.prg 120 quarx.ppm

use std::io::Write;

use c64::C64;
use vic2::{HEIGHT, WIDTH};

fn main() {
    let mut args = std::env::args().skip(1);
    let prg_path = args.next();
    let frames: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(80);
    let out = args.next().unwrap_or_else(|| "screenshot.ppm".into());

    let kernal = std::fs::read("roms/kernal-901227-03.bin").expect("roms/kernal-901227-03.bin");
    let basic = std::fs::read("roms/basic-901226-01.bin").expect("roms/basic-901226-01.bin");
    let chargen = std::fs::read("roms/chargen-901225-01.bin").expect("roms/chargen-901225-01.bin");

    let mut c64 = Box::new(C64::new(&kernal, &basic, &chargen));

    // Boot to READY.
    const READY: [u8; 5] = [0x12, 0x05, 0x01, 0x04, 0x19];
    for i in 0..40_000_000u64 {
        c64.step();
        if i % 100_000 == 0 && c64.board.ram[0x0400..0x07E8].windows(5).any(|w| w == READY) {
            break;
        }
    }

    if let Some(path) = &prg_path {
        let prg = load_prg_bytes(path);
        let addr = c64.load_prg(&prg);
        println!("loaded {path} -> ${addr:04X} ({} bytes)", prg.len());
        if addr == 0x0801 {
            // Type RUN via the KERNAL keyboard buffer.
            c64.board.ram[0x0277..0x027B].copy_from_slice(&[0x52, 0x55, 0x4E, 0x0D]);
            c64.board.ram[0x00C6] = 4;
        }
    }

    let mut fb = vec![0u32; WIDTH * HEIGHT];
    for _ in 0..frames {
        c64.run_frame(&mut fb);
    }

    write_ppm(&out, &fb).expect("write ppm");
    println!("wrote {out} ({WIDTH}x{HEIGHT}) after {frames} frames");
}

fn load_prg_bytes(path: &str) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read program");
    if path.to_ascii_lowercase().ends_with(".d64") {
        let disk = d64::Disk::new(bytes).expect("valid .d64");
        disk.read_prg("*").expect("a PRG on the disk")
    } else {
        bytes
    }
}

fn write_ppm(path: &str, fb: &[u32]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(f, "P6\n{WIDTH} {HEIGHT}\n255\n")?;
    let mut rgb = Vec::with_capacity(fb.len() * 3);
    for &px in fb {
        rgb.push((px >> 16) as u8); // R
        rgb.push((px >> 8) as u8); // G
        rgb.push(px as u8); // B
    }
    f.write_all(&rgb)
}
