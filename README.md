# NES Emulator in Rust

A from-scratch NES emulator. It boots a real cartridge image and plays
**Super Mario Bros.**, **Contra** and **The Legend of Zelda** with picture,
sound, and keyboard controls.

One binary. Run it with no arguments for a small ROM launcher, or point it
straight at a `.nes` file. The launcher remembers its last folder in
`nes-launcher.conf` beside the binary; `nes-players.conf` holds optional
per-game player counts.

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

Point the emulator at a `.nes` file, or at a `.zip` holding one:

```sh
cargo run --release -- "Super Mario Bros. + Duck Hunt (USA).nes"
cargo run --release -- "Contra (USA).nes"
cargo run --release -- "Contra (USA).zip"
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
The launcher lists the `.nes` files and single-ROM `.zip` archives in the last
folder you picked, or in the folder holding the binary on first run. That
folder is remembered in `nes-launcher.conf` next to the binary. Arrow keys move, `Enter` or a click
starts the selected game, typing filters the list by file name, `Backspace`
edits the filter, `F2` opens a folder picker, and `Esc` clears the filter or
quits when there is none. Quitting a game returns to the list.

Starting the binary from a desktop icon works the same way. Naming a ROM on the
command line still skips the launcher.

Bring your own legally dumped ROM. ROM files are not included in this
repository.

## Controls

| NES | Player 1 | Player 2 |
| --- | --- | --- |
| D-pad | Arrow keys | `W` `A` `S` `D` |
| A | `Z` | `G` |
| B | `X` | `H` |
| Select | `Right Shift` | `T` |
| Start | `Enter` | `Y` |

| System | Keyboard |
| --- | --- |
| Pause | `P` |
| Reset | `R` |
| Fast-forward | Hold `Tab` (audio muted) |
| Quicksave slot 1–9 | `1`–`9` |
| Quickload slot 1–9 | `Left Shift`+`1`–`9` |
| Quickload latest slot | `F8` |
| Save-state list | `F7` |
| Fullscreen | `F11` |
| Mute | `M` |
| Quit | `Esc` |

Two SDL2 game controllers work too: the first pad is player 1 and the second
is player 2, each with its D-pad, A, B, Back and Start mapped to the same
buttons (A is the bottom face button, B the right one). Pads can be plugged in
while a game runs; unplugging one frees its player slot. Only two pads are
used. The Zapper is not emulated, so the Duck Hunt half of the cartridge runs
but cannot be aimed.

Games are two-player by default. To make one playable from either keyboard set
or either pad, create `nes-players.conf` beside the binary with one line per
game:

```text
Zelda = 1
Contra (USA) = 2
```

The name is the ROM file name without its extension, the same key the save
folder uses. A `#` starts a comment. A missing file or entry means two players.

## What runs

- **Super Mario Bros.** — playable, with music and sound effects.
- **Contra** — playable on mapper 2 (UxROM).
- **The Legend of Zelda** — playable on mapper 1 (MMC1); progress is saved to
  a `.sav` file beside the ROM periodically while playing and on exit.
- iNES **mapper 0 (NROM)**, **mapper 1 (MMC1)**, **mapper 2 (UxROM)**,
  **mapper 3 (CNROM)**, **mapper 4 (MMC3/TxROM)**, **mapper 7 (AxROM)**,
  **mapper 66** (the SMB/Duck Hunt board), **mapper 71**
  (Camerica/Codemasters) and **mapper 185** (CNROM with a copy-protected
  8 KiB CHR chip) are supported. Mapper 4 covers SMB3, Kirby's
  Adventure, Mega Man 3–6 and most of the TxROM library; mapper 3 covers
  Arkanoid, Adventure Island and other standard CNROM titles; mapper 7 covers
  Battletoads, R.C. Pro-Am and other AxROM titles, including the 512 KiB
  variant; mapper 71 covers MiG 29, Bee 52, Dizzy the Adventurer, the 256 KiB
  Fantastic Adventures of Dizzy and other Camerica titles, with Fire Hawk's
  one-screen mirroring; mapper 185 covers Mighty Bomb Jack, Bird Week and
  other CNROM titles whose CHR chip is disabled until the copy-protection
  latch names its select value. Games on other mappers still exit with a
  mapper error.
- Save states live in `saves/<game>/` beside the binary, one folder per game:
  `state1`–`state9` plus a small `latest` file naming the newest. `<game>` is
  the ROM filename without its extension, so a game's `.nes` and `.zip` share
  one folder -- and two different ROMs with the same filename share one too.
  States an older build wrote beside the ROM move there on first run. A bare
  digit saves, `Left Shift`+digit loads, `F8` loads the most recently saved
  slot. `F7` opens the slot list,
  showing each save's age and whether this ROM can load it; Up/Down choose,
  Enter loads, Esc cancels. A brief message on the frame confirms saves and
  loads; failed save-state loads and writes are shown there too, as well as in
  `nes-errors.log`. A state is tied to the exact ROM and state format version,
  and a bad or foreign file is refused without disturbing the running game.
  Rewind and cheats are not implemented yet.

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

Run the test suite with `cargo test`. `src/golden_frame_test.rs` pins the
picture at selected frames for the bundled test ROMs and, when present, a few
real games under `NES_ROMS/`; a mismatch prints the actual hash to paste back
into the table when the new rendering is intended.
