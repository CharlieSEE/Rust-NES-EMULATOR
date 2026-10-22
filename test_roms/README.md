# Test ROMs

Freely redistributable diagnostic ROMs, committed so the integration tests run
on a clean checkout and in CI instead of skipping.

- `apu_2005/` — blargg's `apu_2005.07.30` frame-counter and length-counter
  suite, with its readme and the assembly source that shipped alongside it.
- `apu_reset/` — blargg's APU reset tests.
- `nestest/` — kevtris's `nestest.nes` and its golden CPU trace; the test runs
  every logged instruction and checks PC, A, X, Y, P, SP and cycles.

Game ROMs are **not** committed. Tests that need one (SMB) skip when it is
absent.
