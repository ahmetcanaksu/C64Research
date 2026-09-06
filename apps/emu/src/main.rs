//! Windowed C64 emulator with sound.
//!
//! Boots the real KERNAL + BASIC, renders the VIC-II with minifb, plays the SID
//! through cpal, and feeds the host keyboard in through a **character-translation
//! layer** — it reads the actual character you type (so it respects your host
//! keyboard layout, e.g. Turkish) and maps it to the matching C64 key + shift.
//! Press Esc to quit.
//!
//! Run from the workspace root:  cargo run -p emu --release

mod status;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use c64::C64;
use harness::keyboard::{self, Typist, RUN_STOP_KEY, SHIFT_KEY};
use harness::{screen, Drive, CYCLES_PER_FRAME};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use minifb::{InputCallback, Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};
use vic2::{HEIGHT, WIDTH};


/// Host key -> (C64 matrix code, needs SHIFT) for keys the host doesn't deliver
/// as characters. The C64 makes cursor-up/left the *shifted* forms of
/// cursor-down/right, and CLR the shifted form of HOME.
const SPECIAL_KEYS: &[(Key, u8, bool)] = &[
    (Key::Down, 7, false),  // CRSR down
    (Key::Up, 7, true),     // CRSR up   = SHIFT + CRSR down
    (Key::Right, 2, false), // CRSR right
    (Key::Left, 2, true),   // CRSR left = SHIFT + CRSR right
    (Key::Home, 51, false), // HOME  (SHIFT+Home would be CLR)
    // DEL comes through here rather than as a character, because whether a
    // control code arrives as a character at all is platform-dependent — see
    // `CharCollector::add_char`. Holding it *should* repeat: INST/DEL is one of
    // the few keys the KERNAL repeats by default.
    (Key::Backspace, 0, false), // INST/DEL — the Mac's main delete key
    (Key::Delete, 0, false),    // INST/DEL — forward delete (fn+Delete)
    // Function keys: F1/F3/F5/F7 are unshifted; F2/F4/F6/F8 are their shifts.
    (Key::F1, 4, false),
    (Key::F2, 4, true),
    (Key::F3, 5, false),
    (Key::F4, 5, true),
    (Key::F5, 6, false),
    (Key::F6, 6, true),
    (Key::F7, 3, false),
    (Key::F8, 3, true),
];

/// Host keys that mean RETURN.
///
/// These are **edge**-triggered in the frame loop, not held like
/// [`SPECIAL_KEYS`], and they are delivered through the type-ahead queue so
/// they get the same press/release discipline as a typed character.
///
/// The distinction matters more than it looks. A held RETURN injects a keypress
/// on every frame it is down, which is fine when you are typing by hand and
/// disastrous when characters are already queued: the stray RETURN submits the
/// line half-typed. Do that to `LOAD"*",8,1` and the C64 runs a bare `LOAD`,
/// which defaults to device 1 — so the machine asks you to PRESS PLAY ON TAPE
/// and never speaks to the disk. The C64 does not repeat RETURN anyway, so
/// there is nothing to lose.
const RETURN_KEYS: &[Key] = &[Key::Enter, Key::NumPadEnter];

/// Physical host key -> C64 keyboard-matrix code, for **direct** (game) mode:
/// the key is set while held and cleared on release, like a real keyboard.
/// Positions follow a QWERTY host, so letters and digits line up with the C64.
/// Arrows and function keys are handled by [`SPECIAL_KEYS`] in both modes.
fn direct_key_matrix(key: Key) -> Option<u8> {
    Some(match key {
        Key::A => 10, Key::B => 28, Key::C => 20, Key::D => 18, Key::E => 14,
        Key::F => 21, Key::G => 26, Key::H => 29, Key::I => 33, Key::J => 34,
        Key::K => 37, Key::L => 42, Key::M => 36, Key::N => 39, Key::O => 38,
        Key::P => 41, Key::Q => 62, Key::R => 17, Key::S => 13, Key::T => 22,
        Key::U => 30, Key::V => 31, Key::W => 9, Key::X => 23, Key::Y => 25,
        Key::Z => 12,
        Key::Key0 => 35, Key::Key1 => 56, Key::Key2 => 59, Key::Key3 => 8,
        Key::Key4 => 11, Key::Key5 => 16, Key::Key6 => 19, Key::Key7 => 24,
        Key::Key8 => 27, Key::Key9 => 32,
        Key::Space => 60,
        Key::Enter | Key::NumPadEnter => 1,
        Key::Backspace | Key::Delete => 0,
        Key::Comma => 47, Key::Period => 44, Key::Slash => 55,
        Key::Semicolon => 50, Key::Equal => 53, Key::Minus => 43,
        _ => return None,
    })
}

type SharedBuf = Arc<Mutex<VecDeque<f32>>>;

fn load_rom(name: &str) -> Result<Vec<u8>, String> {
    std::fs::read(format!("roms/{name}"))
        .map_err(|e| format!("cannot read roms/{name}: {e} (see roms/README.md)"))
}

fn main() -> ExitCode {
    let (kernal, basic, chargen) = match (
        load_rom("kernal-901227-03.bin"),
        load_rom("basic-901226-01.bin"),
        load_rom("chargen-901225-01.bin"),
    ) {
        (Ok(k), Ok(b), Ok(c)) => (k, b, c),
        (k, b, c) => {
            for r in [k, b, c] {
                if let Err(e) = r {
                    eprintln!("error: {e}");
                }
            }
            return ExitCode::FAILURE;
        }
    };

    let mut c64 = Box::new(C64::new(&kernal, &basic, &chargen));
    let mut fb = vec![0u32; WIDTH * HEIGHT];

    // ---- audio ----
    let audio_buf: SharedBuf = Arc::new(Mutex::new(VecDeque::new()));
    let audio = setup_audio(audio_buf.clone());
    let cycles_per_sample = audio.as_ref().map(|(_, sr)| sid::CLOCK_PAL as f32 / *sr as f32);

    // ---- keyboard translation ----
    let char_map = keyboard::char_map();
    let type_queue: Rc<RefCell<VecDeque<char>>> = Rc::new(RefCell::new(VecDeque::new()));

    // ---- info log ----
    println!("Commodore 64  —  PAL");
    println!("  CPU       : MOS 6510 @ 0.985 MHz (verified vs Klaus 6502 test)");
    println!("  video     : MOS 6567 VIC-II, text mode {WIDTH}x{HEIGHT}, scaled 4x");
    println!("  sound     : MOS 6581 SID — 3 voices, ADSR (filter not modeled)");
    match &audio {
        Some((_, sr)) => println!("  audio out : {sr} Hz (see line above for device)"),
        None => println!("  audio out : none found — running silent"),
    }
    println!(
        "  keyboard  : TYPING (host layout → C64, {} chars) — press F12 for DIRECT mode (games)",
        char_map.len()
    );
    let mut clipboard = arboard::Clipboard::new().ok();
    println!(
        "  paste     : {}",
        if clipboard.is_some() {
            "Ctrl+V (or Cmd+V) pastes clipboard text into the C64"
        } else {
            "unavailable"
        }
    );
    println!(
        "  edit keys : Return, arrows = cursor, Home = HOME, Del/Backspace = DEL, F1-F8 = C64 F-keys"
    );
    println!("  joystick  : port 2 = arrow keys, fire = Space / RightCtrl / LeftAlt");
    println!("  break     : Ctrl+C = RUN/STOP    RESTORE = PageUp (Ctrl+C+PageUp = warm reset)");
    println!("  quit      : Esc\n");

    // Optional program or disk:
    //   cargo run -p emu -- <file.prg>                  side-load a raw PRG
    //   cargo run -p emu -- <file.d64> [name]           attach a drive on the bus
    //   cargo run -p emu -- <file.d64> [name] --sideload  skip the bus, inject
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sideload = args.iter().any(|a| a == "--sideload");
    let positional: Vec<&str> =
        args.iter().filter(|a| !a.starts_with("--")).map(String::as_str).collect();

    // A drive on the serial bus, if we attach one. Declared out here because the
    // frame loop below has to clock it alongside the CPU.
    let mut drive: Option<Drive> = None;

    if let Some(&path) = positional.first() {
        let name = positional.get(1).copied();
        let lower = path.to_ascii_lowercase();
        let is_disk = lower.ends_with(".d64");
        let is_cart = lower.ends_with(".bin") || lower.ends_with(".rom") || sideload_cart(&args);
        let no_autoload = args.iter().any(|a| a == "--no-autoload");

        if is_cart {
            // A cartridge is not loaded into the machine, it *is* part of the
            // machine: plugged into the expansion port, then reset so the
            // KERNAL's `$FD02` check can find it and hand over.
            match std::fs::read(path) {
                Ok(rom) => {
                    let cart = if rom.len() > 0x2000 {
                        c64::Cartridge::hi16k(&rom)
                    } else {
                        c64::Cartridge::lo8k(&rom)
                    };
                    let autostart = cart.is_autostart();
                    let kind = if rom.len() > 0x2000 { "16K ($8000+$A000)" } else { "8K ($8000)" };
                    c64.insert_cartridge(cart);
                    println!("  cartridge : {path} — {kind}, {} bytes", rom.len());
                    println!(
                        "  autostart : {}\n",
                        if autostart {
                            "CBM80 found — the cartridge takes over at reset"
                        } else {
                            "no CBM80 signature — BASIC will boot, with less RAM"
                        }
                    );
                }
                Err(e) => eprintln!("  cart error : cannot read {path}: {e}\n"),
            }
        } else if is_disk && !sideload {
            // The real thing: put the disk in a drive, hang the drive off the
            // serial bus, and let the KERNAL fetch the file itself. Slow, the way
            // a 1541 is slow — every byte crosses three wires a bit at a time.
            match std::fs::read(path)
                .map_err(|e| format!("cannot read {path}: {e}"))
                .and_then(|b| d64::Disk::new(b).ok_or_else(|| "not a valid .d64 image".into()))
            {
                Ok(disk) => {
                    // Peek at the load address host-side, purely so we can print
                    // the right hint (RUN for BASIC, SYS for machine code).
                    let want = name.unwrap_or("*");
                    let start = disk
                        .read_prg(want)
                        .filter(|p| p.len() >= 2)
                        .map(|p| u16::from_le_bytes([p[0], p[1]]));

                    let mut attached = Drive::new(disk);
                    println!("  drive     : device 8 on the serial bus <- {path}");
                    run_until_ready(&mut c64, Some(&mut attached));

                    if !no_autoload {
                        for ch in format!("load\"{want}\",8,1\r").chars() {
                            type_queue.borrow_mut().push_back(ch);
                        }
                    }
                    drive = Some(attached);

                    if no_autoload {
                        println!("  ready     : drive attached, nothing typed\n");
                    }
                    match start {
                        _ if no_autoload => {}
                        Some(0x0801) => println!("  loading   : LOAD\"{want}\",8,1  — then type RUN\n"),
                        Some(addr) => {
                            println!("  loading   : LOAD\"{want}\",8,1  — then SYS {addr} to start\n")
                        }
                        None => println!("  loading   : LOAD\"{want}\",8,1\n"),
                    }
                }
                Err(e) => eprintln!("  disk error : {e}\n"),
            }
        } else {
            // Side-load: copy the bytes straight into RAM. No bus involved, so
            // it is instant — handy when you want the program, not the protocol.
            match load_program_file(path, name) {
                Ok(prg) => {
                    let size = prg.len();
                    run_until_ready(&mut c64, None); // boot before injecting
                    let addr = c64.load_prg(&prg);
                    println!("  loaded    : {path} -> ${addr:04X} ({size} bytes, side-loaded)");
                    if addr == 0x0801 {
                        for ch in "run\r".chars() {
                            type_queue.borrow_mut().push_back(ch);
                        }
                        println!("  autostart : RUN\n");
                    } else {
                        println!("  note      : loaded at ${addr:04X}; a machine-code program — SYS {addr} to start\n");
                    }
                }
                Err(e) => eprintln!("  load error : {e}\n"),
            }
        }
    }

    // ---- headless script mode -------------------------------------------
    // No window, no audio: type something, run, print the screen as text. This
    // is how the keyboard and loading paths get exercised from a terminal (and
    // from CI), which matters because "a character went missing" is invisible
    // until you can see the line the C64 actually ended up with.
    if args.iter().any(|a| a == "--headless") {
        let extra = args.iter().find_map(|a| a.strip_prefix("--type="));
        let frames: u32 = args
            .iter()
            .find_map(|a| a.strip_prefix("--frames="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);

        if drive.is_none() {
            // Nothing queued a boot yet, so do it here.
            run_until_ready(&mut c64, None);
        }
        if let Some(text) = extra {
            let mut q = type_queue.borrow_mut();
            for ch in unescape(text).chars() {
                q.push_back(ch);
            }
        }

        let mut typist = Typist::default();
        for _ in 0..frames {
            c64.board.key_matrix = [0; 8];
            typist.frame(&mut c64, &mut type_queue.borrow_mut(), &char_map);
            let deadline = c64.cpu.cycles.wrapping_add(CYCLES_PER_FRAME as u64);
            while c64.cpu.cycles < deadline {
                let cycles = c64.step();
                if let Some(d) = drive.as_mut() {
                    d.tick(&mut c64.board.iec, cycles as u32);
                }
            }
        }

        println!("--- screen ---\n{}\n--- end ---", screen::text(&c64.board.ram));
        if typist.busy() || !type_queue.borrow().is_empty() {
            eprintln!("note: still typing when the frame budget ran out; raise --frames");
        }
        return ExitCode::SUCCESS;
    }

    let mut window = match Window::new(
        "C64 — Esc to quit",
        WIDTH,
        HEIGHT,
        WindowOptions {
            scale: Scale::X4,
            scale_mode: ScaleMode::AspectRatioStretch,
            resize: true,
            ..WindowOptions::default()
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot open window: {e}");
            return ExitCode::FAILURE;
        }
    };
    window.set_target_fps(50);

    // Optional system-monitor window (`--status`): a second window showing CPU,
    // banking, VIC, serial-bus and joystick state, snapshotted once per frame.
    let mut monitor = if args.iter().any(|a| a == "--status") {
        match Window::new(
            "C64 Monitor",
            status::WIDTH,
            status::HEIGHT,
            WindowOptions { scale: Scale::X2, resize: true, ..WindowOptions::default() },
        ) {
            Ok(w) => Some((w, status::Monitor::new())),
            Err(e) => {
                eprintln!("warning: cannot open monitor window: {e}");
                None
            }
        }
    } else {
        None
    };
    let mut frame_count: u32 = 0;

    let debug_keys = args.iter().any(|a| a == "--debug-keys");
    if debug_keys {
        println!("  debug     : logging every character the host delivers\n");
    }
    window.set_input_callback(Box::new(CharCollector {
        queue: type_queue.clone(),
        debug: debug_keys,
    }));

    // One typed character is held down for a couple of frames (so the KERNAL's
    // 60 Hz keyboard scan catches it) then released for a gap frame.
    let mut typist = Typist::default();
    let mut sample_carry = 0.0f32;

    // Two keyboard modes. TYPING (default) buffers keystrokes through a queue and
    // translates the host layout — right for BASIC, LOAD, entering text. But a
    // game reads the raw keyboard matrix, and a queue is exactly wrong for that:
    // it holds each key for a couple of frames and drains a backlog, so a game
    // sees phantom held keys. DIRECT mode maps physical keys straight to the
    // matrix, held only while down — a real keyboard. Toggle with F12.
    let mut kb_direct = false;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let ctrl = window.is_key_down(Key::LeftCtrl)
            || window.is_key_down(Key::RightCtrl)
            || window.is_key_down(Key::LeftSuper)
            || window.is_key_down(Key::RightSuper);

        // F12 switches keyboard mode. Clear any queued typing so it can't leak
        // into a game the instant you switch.
        if window.is_key_pressed(Key::F12, KeyRepeat::No) {
            kb_direct = !kb_direct;
            type_queue.borrow_mut().clear();
            let mode = if kb_direct { "DIRECT (games)" } else { "TYPING" };
            println!("keyboard mode: {mode}");
            window.set_title(&format!("C64 — keyboard: {mode} — Esc to quit"));
        }

        c64.board.key_matrix = [0; 8];
        if kb_direct {
            // Every host key that maps to a C64 key is set while held and cleared
            // on release — no queue, no repeat, no stuck keys.
            for key in window.get_keys() {
                if let Some(code) = direct_key_matrix(key) {
                    c64.board.set_key(code, true);
                }
            }
            if window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift) {
                c64.board.set_key(SHIFT_KEY, true);
            }
        } else {
            // Paste the clipboard into the type-ahead queue (Ctrl+V / Cmd+V).
            if ctrl && window.is_key_pressed(Key::V, KeyRepeat::No) {
                if let Some(cb) = clipboard.as_mut() {
                    if let Ok(text) = cb.get_text() {
                        let mut q = type_queue.borrow_mut();
                        for c in text.chars().take(4096) {
                            q.push_back(c);
                        }
                    }
                }
            }
            // RETURN: one queued '\r' per press, so it can't interleave with
            // characters still being typed.
            if RETURN_KEYS.iter().any(|&k| window.is_key_pressed(k, KeyRepeat::No)) {
                type_queue.borrow_mut().push_back('\r');
            }
            typist.frame(&mut c64, &mut type_queue.borrow_mut(), &char_map);
        }

        // Ctrl+C, held, presses RUN/STOP — the C64 way to break a running
        // program (BASIC prints "BREAK"). Applied live, on top of any typing.
        if ctrl && window.is_key_down(Key::C) {
            c64.board.set_key(RUN_STOP_KEY, true);
        }

        // Editing / special keys the host doesn't send as characters: mapped to
        // the C64 matrix (with SHIFT where the C64 uses a shifted key). Held, so
        // the KERNAL's key-repeat moves the cursor while you hold them.
        for &(host, code, shift) in SPECIAL_KEYS {
            if window.is_key_down(host) {
                c64.board.set_key(code, true);
                if shift {
                    c64.board.set_key(SHIFT_KEY, true);
                }
            }
        }
        // RESTORE is wired to the NMI line, not the matrix — fire it on press.
        if window.is_key_pressed(Key::PageUp, KeyRepeat::No) {
            c64.nmi();
        }

        // Joystick in port 2 (what most games read) from the arrow keys and a
        // fire button. The arrows drive CRSR *and* the joystick — harmless,
        // since a program reads one or the other — so games work either way.
        use c64::joystick as j;
        let mut joy2 = j::CENTER;
        if window.is_key_down(Key::Up) {
            joy2 &= !j::UP;
        }
        if window.is_key_down(Key::Down) {
            joy2 &= !j::DOWN;
        }
        if window.is_key_down(Key::Left) {
            joy2 &= !j::LEFT;
        }
        if window.is_key_down(Key::Right) {
            joy2 &= !j::RIGHT;
        }
        if window.is_key_down(Key::Space)
            || window.is_key_down(Key::RightCtrl)
            || window.is_key_down(Key::LeftAlt)
        {
            joy2 &= !j::FIRE;
        }
        c64.set_joystick(2, joy2);

        // Run one video frame: render each scanline as the raster reaches it (so
        // raster-interrupt splits appear) and sample the SID as we go.
        let mut samples: Vec<f32> = Vec::new();
        let mut rendered = [false; HEIGHT];
        let deadline = c64.cpu.cycles.wrapping_add(CYCLES_PER_FRAME as u64);
        while c64.cpu.cycles < deadline {
            let line = c64.board.vic.raster() as usize;
            if line < HEIGHT && !rendered[line] {
                let cia2 = c64.board.cia2_pra();
                c64.board
                    .vic
                    .render_line(line, &c64.board.ram, &c64.board.chargen, cia2, &mut fb);
                rendered[line] = true;
            }
            let c = c64.step();
            if let Some(d) = drive.as_mut() {
                d.tick(&mut c64.board.iec, c as u32);
            }
            if let Some(cps) = cycles_per_sample {
                sample_carry += c as f32;
                while sample_carry >= cps {
                    sample_carry -= cps;
                    samples.push(c64.board.sid.output());
                }
            }
        }
        // Any scanline the raster didn't reach this pass: render with final state.
        for (y, drawn) in rendered.iter().enumerate() {
            if !drawn {
                let cia2 = c64.board.cia2_pra();
                c64.board
                    .vic
                    .render_line(y, &c64.board.ram, &c64.board.chargen, cia2, &mut fb);
            }
        }
        if let Some((_, sr)) = &audio {
            let mut b = audio_buf.lock().unwrap();
            let cap = (*sr as usize) / 6; // bound latency to ~160 ms
            if b.len() < cap {
                b.extend(samples);
            }
        }

        if let Err(e) = window.update_with_buffer(&fb, WIDTH, HEIGHT) {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }

        // Redraw the monitor window from a fresh per-frame snapshot.
        frame_count = frame_count.wrapping_add(1);
        if let Some((mon_win, mon)) = monitor.as_mut() {
            if mon_win.is_open() {
                mon.render(&c64, drive.is_some(), frame_count);
                let _ = mon_win.update_with_buffer(mon.framebuffer(), status::WIDTH, status::HEIGHT);
            }
        }
    }

    ExitCode::SUCCESS
}

/// Collects the Unicode characters the host types (respecting its layout).
struct CharCollector {
    queue: Rc<RefCell<VecDeque<char>>>,
    /// `--debug-keys`: log every code point the host delivers.
    ///
    /// Worth having permanently. When a key "does nothing" the cause is one of
    /// three things — the host sent no character at all, it sent a character
    /// this build filters out, or it sent one the C64 has no key for — and they
    /// need completely different fixes. Guessing between them from a
    /// description is hopeless; seeing the code point settles it at once.
    debug: bool,
}

impl InputCallback for CharCollector {
    fn add_char(&mut self, uni_char: u32) {
        if self.debug {
            let shown = char::from_u32(uni_char).unwrap_or('?');
            eprintln!(
                "  key       : U+{uni_char:04X} {shown:?}{}",
                if keyboard::is_printable(uni_char) { "" } else { "  (control code — handled as a key)" }
            );
        }
        // Only *printable* characters travel this path. Control codes are
        // handled as keys instead ([`SPECIAL_KEYS`]), because the backends
        // disagree about whether they arrive here at all: minifb's macOS
        // backend drops every code point below 32 (and 127..160) before the
        // callback, while the Windows one passes them straight through. That is
        // why RETURN worked on Windows and did nothing on a Mac.
        //
        // Filtering here rather than compensating per-platform keeps one code
        // path: control keys are always keys, everywhere.
        if !keyboard::is_printable(uni_char) {
            return;
        }
        if let Some(c) = char::from_u32(uni_char) {
            self.queue.borrow_mut().push_back(c);
        }
    }
}

/// Read a program file: a raw `.prg`, or a named PRG extracted from a `.d64`
/// (`name` defaults to the first PRG on the disk).
fn load_program_file(path: &str, name: Option<&str>) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if path.to_ascii_lowercase().ends_with(".d64") {
        let disk = d64::Disk::new(bytes).ok_or_else(|| "not a valid .d64 image".to_string())?;
        let want = name.unwrap_or("*");
        disk.read_prg(want)
            .ok_or_else(|| format!("no PRG '{want}' on the disk"))
    } else {
        Ok(bytes) // treat as a raw .prg
    }
}

/// Step the machine until the KERNAL prints READY. (so an injected program lands
/// at the BASIC prompt). Bounded so a bad ROM can't hang forever.
fn run_until_ready(c64: &mut c64::C64, mut drive: Option<&mut Drive>) {
    const READY: [u8; 5] = [0x12, 0x05, 0x01, 0x04, 0x19];
    for i in 0..40_000_000u64 {
        let cycles = c64.step();
        if let Some(d) = drive.as_deref_mut() {
            d.tick(&mut c64.board.iec, cycles as u32);
        }
        if i % 100_000 == 0 && c64.board.ram[0x0400..0x07E8].windows(5).any(|w| w == READY) {
            return;
        }
    }
}

/// Was `--cart` passed? Lets an image with any extension be treated as one.
fn sideload_cart(args: &[String]) -> bool {
    args.iter().any(|a| a == "--cart")
}

/// Turn `\r` / `\n` / `\\` in a command-line string into real characters, so a
/// shell can ask for a RETURN.
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}


/// Open the default audio output and start streaming from `buf`. Returns the
/// live stream (keep it alive) and the sample rate, or `None` if unavailable.
fn setup_audio(buf: SharedBuf) -> Option<(cpal::Stream, u32)> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let name = device.name().unwrap_or_else(|_| "unknown".into());
    let config = device.default_output_config().ok()?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let fmt = config.sample_format();
    println!("  audio dev : {name} — {sample_rate} Hz, {channels} ch, {fmt:?}");

    let cfg: cpal::StreamConfig = config.into();
    let stream = match fmt {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &cfg, buf),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &cfg, buf),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &cfg, buf),
        _ => None,
    }?;
    stream.play().ok()?;
    Some((stream, sample_rate))
}

fn build_stream<T>(device: &cpal::Device, cfg: &cpal::StreamConfig, buf: SharedBuf) -> Option<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels as usize;
    device
        .build_output_stream(
            cfg,
            move |data: &mut [T], _| {
                let mut b = buf.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let s = b.pop_front().unwrap_or(0.0);
                    let v = T::from_sample(s);
                    for ch in frame.iter_mut() {
                        *ch = v;
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host-key bindings, which are the only keyboard concern left in this
    /// binary — the character mapping, the typing discipline and the end-to-end
    /// load tests all live in `crates/harness`.
    #[test]
    fn return_is_edge_triggered_and_delete_is_held() {
        let held =
            |k: Key| SPECIAL_KEYS.iter().find(|&&(key, _, _)| key == k).map(|&(_, c, s)| (c, s));

        // A Mac's main delete key is Backspace; fn+Delete is Delete. Both are the
        // C64's single INST/DEL, and both are *held* so they repeat — INST/DEL is
        // one of the few keys the KERNAL repeats by default.
        assert_eq!(held(Key::Backspace), Some((0, false)), "INST/DEL (backspace)");
        assert_eq!(held(Key::Delete), Some((0, false)), "INST/DEL (forward delete)");

        // RETURN must NOT be held: a held RETURN fires on every frame it is down
        // and would submit a line that is still being typed.
        assert!(RETURN_KEYS.contains(&Key::Enter), "RETURN must be bound");
        assert!(RETURN_KEYS.contains(&Key::NumPadEnter), "keypad RETURN must be bound");
        for k in RETURN_KEYS {
            assert_eq!(held(*k), None, "{k:?} must not also be a held key");
        }
    }

    /// `--type` has to be able to ask for a RETURN from a shell.
    #[test]
    fn unescapes_command_line_escapes() {
        assert_eq!(unescape(r"run\r"), "run\r");
        assert_eq!(unescape(r"a\nb"), "a\nb");
        assert_eq!(unescape(r"back\\slash"), r"back\slash");
        assert_eq!(unescape("plain"), "plain");
        // An unknown escape is left alone rather than eaten.
        assert_eq!(unescape(r"\q"), r"\q");
    }
}
