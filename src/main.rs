use sdl2::event::Event;
use sdl2::keyboard::Keycode;

mod bus;
mod controller;
mod cpu;
mod ppu;
mod rom;

use bus::Bus;
use cpu::Cpu;
use rom::Cartridge;

const W: usize = 256;
const H: usize = 240;
const SCALE: u32 = 3;

/// Where `--dump N` writes its frame.
const DUMP_PATH: &str = "frame.ppm";

const USAGE: &str = "usage: nes <rom.nes> [--dump N]\n\
    \x20 <rom.nes>  the cartridge to boot, quoted if it has spaces\n\
    \x20 --dump N   after N frames write frame.ppm and exit (no extra dependency)";

fn main() -> Result<(), String> {
    let mut args = ::std::env::args().skip(1);
    let rom_path = args.next().ok_or_else(|| USAGE.to_string())?;
    let dump_after: Option<u32> = match args.next().as_deref() {
        None => None,
        Some("--dump") => Some(
            args.next()
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("--dump needs a frame count\n{USAGE}"))?,
        ),
        Some(other) => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
    };

    let sdl = sdl2::init().map_err(|e| e.to_string())?;
    let video = sdl.video().map_err(|e| e.to_string())?;
    let window = video
        .window("NES", (W as u32) * SCALE, (H as u32) * SCALE)
        .position_centered()
        .allow_highdpi()
        .build()
        .map_err(|e| e.to_string())?;
    let mut canvas = window.into_canvas().build().map_err(|e| e.to_string())?;
    canvas
        .set_logical_size(W as u32, H as u32)
        .map_err(|e| e.to_string())?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture(
            sdl2::pixels::PixelFormatEnum::ARGB8888,
            sdl2::render::TextureAccess::Streaming,
            W as u32,
            H as u32,
        )
        .map_err(|e| e.to_string())?;

    let bytes = ::std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
    let mut bus = Bus::new(Cartridge::load(&bytes)?);
    // The PPU fetches pattern memory every dot, so the CHR ROM the cartridge
    // parsed out of the file is loaded once into the PPU's banks.
    bus.ppu.load_chr_rom(bus.cart.chr_rom());
    let mut cpu = Cpu::new();
    bus.reset();
    cpu.reset(&mut bus);

    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    // Which controller bit a held key presses, in `Controller::set_buttons` order:
    // A, B, Select, Start, Up, Down, Left, Right.
    fn button_for(key: Keycode) -> Option<u8> {
        Some(match key {
            Keycode::Z => 0x01,      // A
            Keycode::X => 0x02,      // B
            Keycode::RShift => 0x04, // Select
            Keycode::Return => 0x08, // Start
            Keycode::Up => 0x10,
            Keycode::Down => 0x20,
            Keycode::Left => 0x40,
            Keycode::Right => 0x80,
            _ => return None,
        })
    }

    let mut buttons = 0u8;
    let mut running = true;
    let mut frames = 0u32;
    while running {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => running = false,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => running = false,
                Event::KeyDown {
                    keycode: Some(key), ..
                } => {
                    if let Some(bit) = button_for(key) {
                        buttons |= bit;
                    }
                }
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(bit) = button_for(key) {
                        buttons &= !bit;
                    }
                }
                _ => {}
            }
        }
        bus.controllers[0].set_buttons(buttons);
        // Run the CPU until the PPU has a whole frame, taking the NMI the
        // vblank flag raises. SMB gates its status bar on a sprite-0 hit and
        // its NMI handler on `$2002`, so this is where the machine actually
        // lives; Task 7 takes the timing and audio from here.
        while !bus.ppu.frame_ready() {
            // An OAM DMA halts the CPU for its 513 cycles while the PPU keeps
            // clocking; `tick` is one dot, so three of them per cycle drain it.
            if bus.dma_busy() {
                bus.tick();
                continue;
            }
            cpu.step(&mut bus);
            if bus.ppu.nmi_pending() {
                bus.ppu.take_nmi();
                cpu.nmi(&mut bus);
            }
        }
        // The 2A03's maskable interrupt is the APU's and only the APU's, and
        // `$4015` bits 7-6 are its two request flags (NESdev "APU", $4015 read:
        // "IF-D NT21"). They read 0 here because there is no APU, so this never
        // fires.
        //
        // ponytail: polled once a frame, not per instruction. Reading $4015
        // clears the frame-counter flag, so a per-instruction poll would eat the
        // IRQ before the APU task can deliver it; swap this for a level-triggered
        // IRQ line the APU raises when there is one.
        if bus.cpu_read(0x4015) & 0xC0 != 0 {
            cpu.irq(&mut bus);
        }
        // `take_frame` is what clears the ready flag, and it is the ONLY thing that
        // does -- without this the loop above exits once and never runs again.
        // Its returned clone is dropped; the upload reads the PPU's own buffer
        // by reference instead, so a frame costs one 240 KiB copy rather than
        // two.
        let _ = bus.ppu.take_frame();
        let frame = bus.ppu.framebuffer();
        frames += 1;
        if dump_after == Some(frames) {
            write_ppm(DUMP_PATH, frame)?;
            return Ok(());
        }
        // SAFETY: u32 is plain old data and every bit pattern is a valid u32, so
        // all len*4 bytes of the frame are valid to read as u8.
        let frame_bytes =
            unsafe { ::std::slice::from_raw_parts(frame.as_ptr() as *const u8, frame.len() * 4) };
        texture
            .update(None, frame_bytes, W * 4)
            .map_err(|e| e.to_string())?;
        canvas.clear();
        canvas
            .copy(&texture, None, None)
            .map_err(|e| e.to_string())?;
        canvas.present();
        ::std::thread::sleep(::std::time::Duration::from_micros(16_667));
    }
    Ok(())
}

/// One framebuffer pixel is `0x00RRGGBB` (see `Ppu::colour`), and binary PPM
/// `P6` wants three bytes per pixel in R, G, B order, so the only work is
/// dropping the byte the mask left clear. Convert to PNG outside the crate --
/// `sdl2` is the only dependency this project has.
fn write_ppm(path: &str, frame: &[u32]) -> Result<(), String> {
    let rgb: Vec<u8> = frame
        .iter()
        .flat_map(|&px| [(px >> 16) as u8, (px >> 8) as u8, px as u8])
        .collect();
    let mut out = Vec::with_capacity(rgb.len() + 32);
    out.extend_from_slice(format!("P6\n{W} {H}\n255\n").as_bytes());
    out.extend_from_slice(&rgb);
    ::std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
}
