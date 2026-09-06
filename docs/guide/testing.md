# testing

`compose_test` runs a composition headlessly: the same runtime and the same native layout as a
window, with no window and deterministic frames

```by
from basedpython_ui import composable, state, Column, Text, Button, compose_test


@composable
def Counter(step: int = 1):
    let count = state(0)
    Column:
        Text(f"count = {count.value}")
        Button("+"):
            count.value += step


def test_counter_increments():
    let t = compose_test:                       # headless: same runtime, same native layout, no window
        Counter(step=2)
    assert t.find_text("count = 0")
    t.click("+")
    t.advance()                                 # run pending recompositions, commit, layout, effects
    assert t.find_text("count = 2")
    assert t.recompositions(Counter) == 2       # the initial composition and exactly one recomposition
```

`examples/test_counter.by` is that test, runnable on its own

## the harness

`compose_test(width=400, height=300, root)` builds a `Core` of that size, a `Runtime` over it, sets
the root and advances once, so the first composition, commit and layout have happened by the time
it returns. the block binds `root`. tracing is on unless `trace=False` is passed

| call | does |
| --- | --- |
| `advance(steps=8)` | runs frames — posted writes, pending recompositions, one commit, layout, effects — until the runtime is idle or `steps` frames have run, letting queued tasks make progress between frames |
| `find_text(text) -> bool` | whether a node shows exactly `text`: a `Text`, a `Button` label, a `TextField` value, or a placeholder |
| `all_text() -> (*: str)` | every visible text in tree order |
| `click(label)` | press and release the pointer over the node showing `label`, through the same hit test and event path a window uses; raises `AssertionError` with the tree dump when nothing shows `label` or it is not clickable |
| `type_into(placeholder, text)` | focus the field showing `placeholder` (or its current value) and type `text` |
| `recompositions(fn) -> int` | how many times scopes of composable `fn` have run, the initial composition included |
| `why(fn) -> (*: Recomposition)` | the trace's run records of `fn`'s scopes, matched by identity, oldest first; each has `.frame`, `.scope`, `.origin`, `.causes` (a tuple of `Cause`), `.skipped`, `.disposed`, `.defined`, `.called`, `.elapsed_ns` and `.text()` — [why did this rerender](why-did-this-rerender.md#the-python-api) |
| `explain(since_frame=None) -> str` | the whole ring rendered as text, one entry per record, writes and frames included; `since_frame` keeps only records from that frame on |
| `runtime.trace` | the `Trace` itself: `records`, `dropped`, `limit`, `frames`; `None` when tracing is off |
| `dump() -> str` | the core's retained tree, one line per node with kinds, text and rects |
| `dispose()` | dispose the root scope, remove the runtime from `live_runtimes` and close the harness's event loop; `with compose_test(...) as t:` does it on exit |

`click` and `type_into` fail with the dump rather than silently doing nothing, so a test that
clicks a label that is not there reads the tree it did get

## the fake core and the real one

the runtime tests, `tests/test_runtime.by`, drive `Runtime` against `FakeCore`: a python class that
keeps the retained tree as one record list per scope group exactly as the
[native core protocol](../development/native-protocol.md) describes — enough to answer
`find_text`, to count what each commit carried and to say which scope groups were disposed. no
rust, no window. that is where the runtime's behaviour is pinned: skipping and its counts, keyed
children surviving removal, deriveds recomputing once and notifying only on change, a write during
composition refused with the tree kept, a mutable value refused at entry, ambients, effects, a
write from another thread posted rather than applied, a failing child keeping its previous subtree,
and eight worker threads appending without a loss. `tests/test_examples.by` composes every example
against the same fake core

`tests/test_harness.by` is `compose_test`, so the real core: composition, commit, layout, hit
testing and text editing end to end, including a custom `Layout` and a `Canvas` through the rust
side. it needs the extension built first

on 2026-09-05 the suite is 21 tests: 18 against the fake core, 3 against the real one

## the commands

the examples type-check with the compiler fork and build through it; `by check` runs every lint,
`by build --soundness none` skips the runtime soundness wrappers that cost about 30 % on
composition. the fork is a worktree of the basedpython checkout beside this one:

```bash
/Users/morgan/projects/basedpython/.claude/worktrees/ui-support-adversarial-review-39602f/target/debug/by check
```

```bash
/Users/morgan/projects/basedpython/.claude/worktrees/ui-support-adversarial-review-39602f/target/debug/by build --soundness none
```

build the rust core once (release; copies the extension into `src/basedpython_ui/`, from where
`by build` carries it into `out/`):

```bash
native/build.sh
```

then the tests and the composition benchmark run from the build output:

```bash
cd out && ../.venv/bin/python -m pytest tests -q
```

```bash
cd out && ../.venv/bin/python -m bench.compose_bench
```

the same suite passes on free-threaded python. `native/build.sh` builds a second extension when
`PYO3_PYTHON=python3.14t`, and the two `.so` files coexist:

```bash
cd out && uv run --no-project --python 3.14t --with pytest python -m pytest tests -q
```

an example in a window:

```bash
cd out && ../.venv/bin/python -c "from examples import counter; counter.main()"
```
