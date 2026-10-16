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

/// The cartridge this emulator is built for, next to `Cargo.toml`.
const ROM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/Super Mario Bros. + Duck Hunt (USA).nes"
);

const W: usize = 256;
const H: usize = 240;
const SCALE: u32 = 3;

fn main() -> Result<(), String> {
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

    let bytes = ::std::fs::read(ROM_PATH).map_err(|e| format!("{}: {e}", ROM_PATH))?;
    let mut bus = Bus::new(Cartridge::load(&bytes)?);
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
