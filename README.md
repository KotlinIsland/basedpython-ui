# basedpython-ui

a compose / swiftui / flutter inspired ui framework for [basedpython](../basedpython).

```by
@composable
def Counter(step: int = 1):
    let count = state(0)
    Column(Modifier().padding(16)):
        Text(f"count = {count.value}")
        Row:
            Button("-", enabled=count.value > 0):
                count.value -= step
            Button("+"):
                count.value += step
```

- **type-safe mutation**: only immutable values (frozen data classes, scalars, tuples, enum
  variants) or observable containers (`StateList`, `StateDict`) can be held in state, and the
  compiler fork reports any in-place mutation a render could not observe, and any read during
  composition of a value nothing observes (a `list` parameter, a mutable global), wherever the
  write lives
- **inferred dependencies**: the editor shows what each composable reads
  (`def Counter(step: int = 1)⟨ reads count⟩:`), what each `derived` depends on, and which
  parameters are `⟨unstable ⟩` — inferred by the compiler fork, rendered by `by server`
- **fast where it matters**: composition and state in basedpython, layout / text / paint / input in
  a rust core, one batched commit per frame

read [docs/design.md](docs/design.md), and [docs/lints.md](docs/lints.md) for what the compiler fork says about a file of deliberate mistakes. the examples in [examples/](examples) type-check with the
compiler fork (branch `ui-support`, a worktree of the basedpython checkout; the examples rely on
its fixes) and run through the runtime:

```bash
/Users/morgan/projects/basedpython/.claude/worktrees/ui-support/target/debug/by check
```

```bash
/Users/morgan/projects/basedpython/.claude/worktrees/ui-support/target/debug/by build --soundness none
```

build the rust core once (release, copies the extension into `src/basedpython_ui/`):

```bash
native/build.sh
```

then the tests (17 against a fake native core, 3 against the real one) and the composition
benchmark run from the build output:

```bash
cd out && ../.venv/bin/python -m pytest tests -q
```

```bash
cd out && ../.venv/bin/python -m bench.compose_bench
```

the same suite passes on free-threaded python (`native/build.sh` builds a second extension when
`PYO3_PYTHON=python3.14t`; the two `.so` files coexist):

```bash
cd out && uv run --no-project --python 3.14t --with pytest python -m pytest tests -q
```

an example in a window:

```bash
cd out && ../.venv/bin/python -c "from examples import counter; counter.main()"
```

layout:

- `src/basedpython_ui/` — the framework (`runtime.by`: cells, scopes, slots, effects, ambients;
  `widgets.by`: builders emitting fragments; `app.by`: `run_app` and `compose_test`)
- `native/` — the rust core (pyo3): retained tree, layout, text, paint, input, window
- `examples/` — the mockups, each a runnable app
- `tests/`, `bench/` — verified against a fake core; `docs/` — design and the native protocol
