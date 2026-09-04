# native

the rust core of basedpython-ui: element arena, reconciliation, layout, text, painting, hit
testing and the window. the contract is [`docs/native-protocol.md`](../docs/native-protocol.md)
(read its amendments and decisions sections too).

## build

```sh
native/build.sh          # release build, copies src/basedpython_ui/_native.cpython-314-darwin.so
```

needs the nightly toolchain (`cargo 1.97`) and `python3.14` on the path (`PYO3_PYTHON` overrides
it; `.cargo/config.toml` sets the default and the macOS link flags). the module imports as
`basedpython_ui._native`; the `.so` is gitignored.

## test

```sh
cd native && cargo test --no-default-features          # rust unit tests, no libpython needed
cd .. && PYTHONPATH=src .venv/bin/python -m pytest native/tests -q -s   # boundary tests (prints the 10k-node timing)
```

`PYTHONPATH=src python3.14 -m pytest native/tests -q` works too once that interpreter has pytest.

## layout of the crate

| file | what |
|---|---|
| `src/types.rs` | value types: kinds, keys, constraints, modifiers, styles, canvas commands |
| `src/tree.rs` | the arena (`SlotMap`), scope groups, interned tables, focus, `dump` / `find_text` |
| `src/text.rs` | cosmic-text shaping + cache, glyph rasterisation, monospace fallback |
| `src/commit.rs` | validation (all checks, then no mutation on error) and reconciliation |
| `src/layout.rs` | constraints-down / sizes-up layout, size cache, the `MeasureHost` callback path |
| `src/paint.rs` | tiny-skia painting into the RGBA buffer |
| `src/input.rs` | hit testing, pointer / key events, the text-field edit buffer |
| `src/py.rs` | the `Core` pyclass: panic catching, poisoning, the re-entrant layout callback |
| `src/window.rs` | the `Window` pyclass: winit 0.30 `ApplicationHandler` + softbuffer blit |

## what is simplified in M1

- every frame repaints everything (no damage rects / repaint boundaries, no GPU) — TODOs in
  `paint.rs`.
- text fields: single line, caret, insert / delete / arrows / home / end, tab cycles fields,
  escape blurs; no selection, clipboard or IME composition.
- buttons and fields are flat rectangles; no rounded corners, hover or pressed states.
- canvas commands are not clipped to the canvas.
- one window (winit allows one event loop per process); the loop must run on the main thread.
