use sdl2::event::Event;
use sdl2::keyboard::Keycode;

mod rom;

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

    let mut buffer = vec![0u32; W * H];
    for p in buffer.iter_mut() {
        *p = 0x00_3A_6E_32;
    }
    let mut events = sdl.event_pump().map_err(|e| e.to_string())?;
    // SAFETY: u32 is plain old data and every bit pattern is a valid u32, so all
    // len*4 bytes of the framebuffer are valid to read as u8.
    let bytes =
        unsafe { ::std::slice::from_raw_parts(buffer.as_ptr() as *const u8, buffer.len() * 4) };
    let mut running = true;
    while running {
        for event in events.poll_iter() {
            if matches!(
                event,
                Event::Quit { .. }
                    | Event::KeyDown {
                        keycode: Some(Keycode::Escape),
                        ..
                    }
            ) {
                running = false;
            }
        }
        texture
            .update(None, bytes, W * 4)
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
