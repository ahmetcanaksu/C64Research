//! Windowed C64 emulator with sound.
//!
//! Boots the real KERNAL + BASIC, renders the VIC-II with minifb, plays the SID
//! through cpal, and feeds the host keyboard in through a **character-translation
//! layer** — it reads the actual character you type (so it respects your host
//! keyboard layout, e.g. Turkish) and maps it to the matching C64 key + shift.
//! Press Esc to quit.
//!
//! Run from the workspace root:  cargo run -p emu --release

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use c64::C64;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use minifb::{InputCallback, Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};
use vic2::{HEIGHT, WIDTH};

/// PAL C64: ~985 kHz / 50 Hz ≈ 19700 CPU cycles per frame.
const CYCLES_PER_FRAME: u32 = 19_700;
/// SHIFT's key in the C64 matrix.
const SHIFT_KEY: u8 = 15;
/// RUN/STOP's key in the C64 matrix (matrix code 63) — breaks a running program.
const RUN_STOP_KEY: u8 = 63;

/// Host key -> (C64 matrix code, needs SHIFT) for keys the host doesn't deliver
/// as characters. The C64 makes cursor-up/left the *shifted* forms of
/// cursor-down/right, and CLR the shifted form of HOME.
const SPECIAL_KEYS: &[(Key, u8, bool)] = &[
    (Key::Down, 7, false),   // CRSR down
    (Key::Up, 7, true),      // CRSR up   = SHIFT + CRSR down
    (Key::Right, 2, false),  // CRSR right
    (Key::Left, 2, true),    // CRSR left = SHIFT + CRSR right
    (Key::Home, 51, false),  // HOME  (SHIFT+Home would be CLR)
    (Key::Delete, 0, false), // INST/DEL
];

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
    let char_map = build_char_map();
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
        "  keyboard  : character-translation layer — your host layout → C64 matrix ({} chars mapped)",
        char_map.len()
    );
    let mut clipboard = arboard::Clipboard::new().ok();
    println!(
        "  paste     : {}",
        if clipboard.is_some() { "Ctrl+V pastes clipboard text into the C64" } else { "unavailable" }
    );
    println!("  edit keys : arrows = cursor, Home = HOME, Del/Backspace = DEL");
    println!("  break     : Ctrl+C = RUN/STOP    RESTORE = PageUp (Ctrl+C+PageUp = warm reset)");
    println!("  quit      : Esc\n");

    // Optional program to load:  cargo run -p emu -- <file.prg|file.d64> [name]
    let mut cli = std::env::args().skip(1);
    if let Some(path) = cli.next() {
        let name = cli.next();
        match load_program_file(&path, name.as_deref()) {
            Ok(prg) => {
                let size = prg.len();
                run_until_ready(&mut c64); // boot before injecting
                let addr = c64.load_prg(&prg);
                println!("  loaded    : {path} -> ${addr:04X} ({size} bytes)");
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
    window.set_input_callback(Box::new(CharCollector { queue: type_queue.clone() }));

    // One typed character is held down for a couple of frames (so the KERNAL's
    // 60 Hz keyboard scan catches it) then released for a gap frame.
    let mut hold: Option<(u8, bool, u8)> = None;
    let mut gap = 0u8;
    let mut sample_carry = 0.0f32;

    while window.is_open() && !window.is_key_down(Key::Escape) {
        // Ctrl+V: push the clipboard text into the type-ahead queue.
        let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
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

        // Drive the keyboard matrix from the injection state machine.
        c64.board.key_matrix = [0; 8];
        if let Some((code, shift, left)) = hold {
            c64.board.set_key(code, true);
            if shift {
                c64.board.set_key(SHIFT_KEY, true);
            }
            hold = if left > 1 {
                Some((code, shift, left - 1))
            } else {
                gap = 1;
                None
            };
        } else if gap > 0 {
            gap -= 1;
        } else if let Some(c) = type_queue.borrow_mut().pop_front() {
            if let Some(&(code, shift)) = char_map.get(&c) {
                c64.board.set_key(code, true);
                if shift {
                    c64.board.set_key(SHIFT_KEY, true);
                }
                hold = Some((code, shift, 1));
            }
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
            if let Some(cps) = cycles_per_sample {
                sample_carry += c as f32;
                while sample_carry >= cps {
                    sample_carry -= cps;
                    samples.push(c64.board.sid.output());
                }
            }
        }
        // Any scanline the raster didn't reach this pass: render with final state.
        for y in 0..HEIGHT {
            if !rendered[y] {
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
    }

    ExitCode::SUCCESS
}

/// Collects the Unicode characters the host types (respecting its layout).
struct CharCollector {
    queue: Rc<RefCell<VecDeque<char>>>,
}
impl InputCallback for CharCollector {
    fn add_char(&mut self, uni_char: u32) {
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
fn run_until_ready(c64: &mut c64::C64) {
    const READY: [u8; 5] = [0x12, 0x05, 0x01, 0x04, 0x19];
    for i in 0..40_000_000u64 {
        c64.step();
        if i % 100_000 == 0 && c64.board.ram[0x0400..0x07E8].windows(5).any(|w| w == READY) {
            return;
        }
    }
}

/// PETSCII byte -> ASCII-range char (the two coincide over $20..=$5F).
fn petscii_char(b: u8) -> Option<char> {
    match b {
        0x20..=0x5F => Some(b as char),
        _ => None,
    }
}

/// Build a character -> (C64 matrix code, needs-shift) map straight from the
/// KERNAL's own keyboard decode tables, so the mapping is exactly the C64's.
fn build_char_map() -> HashMap<char, (u8, bool)> {
    use kernal::irq::{SHIFTED_KEYS, UNSHIFTED_KEYS};
    let mut m = HashMap::new();
    for idx in 0..64usize {
        if let Some(ch) = petscii_char(UNSHIFTED_KEYS[idx]) {
            m.entry(ch).or_insert((idx as u8, false));
            if ch.is_ascii_uppercase() {
                m.entry(ch.to_ascii_lowercase()).or_insert((idx as u8, false));
            }
        }
        if let Some(ch) = petscii_char(SHIFTED_KEYS[idx]) {
            m.entry(ch).or_insert((idx as u8, true));
        }
    }
    // Non-printing keys the host still sends as characters.
    m.insert('\r', (1, false)); // RETURN
    m.insert('\n', (1, false));
    m.insert('\u{8}', (0, false)); // Backspace -> DEL
    m.insert('\u{7f}', (0, false));
    m
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
