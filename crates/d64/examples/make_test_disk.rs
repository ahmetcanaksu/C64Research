//! Write a small, valid `.d64` you can actually boot from.
//!
//! The C64 ROMs can't be committed and neither can anyone's game disks, so this
//! builds a disk from scratch instead: a formatted image holding one BASIC
//! program. Handy for exercising the serial-bus path end to end without needing
//! a real disk image from somewhere.
//!
//! ```sh
//! cargo run -p d64 --example make-test-disk -- test.d64
//! cargo run -p emu --release -- test.d64        # LOAD over the bus, then RUN
//! ```

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "test.d64".to_string());
    let minimal = args.any(|a| a == "--minimal");

    let image = if minimal {
        // Three dummy bytes — for testing a transfer, not for running.
        d64::fixtures::synthetic_image()
    } else {
        // 10 PRINT"HELLO FROM DISK"
        d64::fixtures::basic_program_image()
    };

    match std::fs::write(&path, &image) {
        Ok(()) => {
            println!("wrote {path} ({} bytes)", image.len());
            println!("  disk name : TEST DISK, id 01, DOS 2A");
            println!("  file      : HELLO (PRG)");
            if !minimal {
                println!("\ntry:  cargo run -p emu --release -- {path}");
                println!("      ...then type RUN");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: cannot write {path}: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
