# NES Emulator in Rust

A from-scratch NES emulator. It boots a real cartridge image and plays
**Super Mario Bros.**, **Contra** and **The Legend of Zelda** with picture,
sound, and keyboard controls.

One binary. Run it with no arguments for a small ROM launcher, or point it
straight at a `.nes` file. The launcher's only setting, its last folder, lives
in `nes-launcher.conf` beside the binary.

## Requirements

- Rust (stable, via [rustup](https://rustup.rs))
- SDL2 development libraries
- GTK3 development libraries (for the launcher's folder picker)

```sh
# Debian / Ubuntu
sudo apt install libsdl2-dev libgtk-3-dev
```

Build with:

```sh
cargo build --release
```

## Play

Point the emulator at a `.nes` file:

```sh
cargo run --release -- "Super Mario Bros. + Duck Hunt (USA).nes"
cargo run --release -- "Contra (USA).nes"
cargo run --release -- "Legend of Zelda, The (Europe) (Rev 1).nes"
```

Or run the built binary directly:

```sh
./target/release/nes "Super Mario Bros. + Duck Hunt (USA).nes"
```

Quote paths that contain spaces. The window is resizable; the frame scales to
fit it. Audio goes to the default output device, and a machine with no audio
device runs silently instead of failing to start.

### Launcher

Run the binary with no arguments:

```sh
./target/release/nes
```

The launcher lists the `.nes` files in the last folder you picked, or in the
folder holding the binary on first run. That folder is remembered in
`nes-launcher.conf` next to the binary. Arrow keys move, `Enter` or a click
starts the selected game, `O` opens a folder picker, and `Esc` quits. Quitting
a game returns to the list.

Starting the binary from a desktop icon works the same way. Naming a ROM on the
command line still skips the launcher.

Bring your own legally dumped ROM. ROM files are not included in this
repository.

## Controls

| NES | Keyboard |
| --- | --- |
| D-pad | Arrow keys |
| A | `Z` |
| B | `X` |
| Select | `Right Shift` |
| Start | `Enter` |
| Pause | `P` |
| Reset | `R` |
| Quicksave slot 1–9 | `1`–`9` |
| Quickload slot 1–9 | `Left Shift`+`1`–`9` |
| Quickload latest slot | `F8` |
| Fullscreen | `F11` |
| Mute | `M` |
| Quit | `Esc` |

An SDL2 game controller works too: its D-pad, A, B, Back and Start map to the
same buttons (A is the bottom face button, B the right one). One player,
standard controller only. The Zapper is not emulated, so the Duck Hunt half of
the cartridge runs but cannot be aimed.

## What runs

- **Super Mario Bros.** — playable, with music and sound effects.
- **Contra** — playable on mapper 2 (UxROM).
- **The Legend of Zelda** — playable on mapper 1 (MMC1); progress is saved to
  a `.sav` file beside the ROM periodically while playing and on exit.
- iNES **mapper 0 (NROM)**, **mapper 1 (MMC1)**, **mapper 2 (UxROM)** and
  **mapper 66** (the SMB/Duck Hunt board) are supported. Most other games will
  not boot and will exit with a mapper error.
- Save states live beside the ROM as `.state1`–`.state9` files, with a small
  `.state` index naming the newest: a bare digit saves, `Left Shift`+digit
  loads, `F8` loads the most recently saved slot. A state is tied to the exact
  ROM and state format version, and a bad or foreign file is refused without
  disturbing the running game. Rewind and cheats are not implemented yet.

## For tinkerers

The binary has two flags that make screenshots and scripted runs easy:

```sh
# Write frame-600.ppm and frame-1400.ppm, then exit after frame 1400
./target/release/nes "game.nes" --dump 600,1400

# Press Start once, wait a second, then hold Right for 120 frames
./target/release/nes "game.nes" --play START,WAIT60,RIGHT120
```

Button names are `A`, `B`, `SELECT`, `START`, `UP`, `DOWN`, `LEFT`, `RIGHT`,
plus the aliases `JUMP` (A), `RUN` (B), and `FIRE` (B). Join buttons with `+`
to hold several at once (`RIGHT+A30`), and append a frame count to any step.

Run the test suite with `cargo test`.
