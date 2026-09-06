# why did this rerender

a composable re-ran, and the reasons it can have are few: a cell it read changed, its parent ran
it with different arguments, or the runtime had a structural reason — it was new, its key changed,
it takes a content block and so runs with its parent, it was recovering from an error. a count of
recompositions cannot tell those apart, and a count is all `recompositions(Counter)` gives

so the runtime keeps a **record** of every scope run: which scope, in which frame, why, what it
skipped, what it disposed, and how long its body took. beside it sit records of every state write,
of every frame, of every error and of every refused write. the records live in a bounded ring on
the runtime (`Runtime.trace`), and three readers share it:

- in process: `Runtime.explain()` and `TestComposition.why(fn)` — [the python api](#the-python-api)
- the debugger, `bpd`: `bpd/recompositions` over DAP and `recompositions` over MCP read the ring at
    a stop; `bpd/watchRecompositions` and `watch_recompositions` stream records as they are appended
    — [the debugger](debugger.md)
- pycharm, through `bpd`: a tool window, and a label on the composable's header line

the record layout is the [trace protocol](../development/trace-protocol.md), and a change to it
bumps `TRACE_FORMAT`; this page is what the records mean

## the run record

a run record is a tuple whose first element is `1`:

```text
(1, frame, scope_id, parent_id, name, def_file, def_line, call_file, call_line, key,
    origin, causes, skipped, disposed, elapsed_ns)
```

`frame` is the runtime's frame counter when the run started. `scope_id` is the scope's id (the root
is 0, its parent `None`) and `name` the composable's `__name__` (`root` for the root scope);
`def_file` / `def_line` is where the composable is defined and `call_file` / `call_line` where its
parent called it, both as the generated `.py` the interpreter runs. `key` is the enclosing
`key(...)` value or `None`. `elapsed_ns` is the wall time of the body. `origin` says how the run
started:

| origin | the run started because |
| --- | --- |
| `first` | the scope was created this run |
| `self` | the scope was dirty and was popped from the dirty heap |
| `parent` | its parent ran, reached it, and could not skip it |

and `causes` says why, as a tuple of one or more cause tuples. it is never empty: a run with no
reason would be a bug, not a record

## the causes

| cause | carries | when |
| --- | --- | --- |
| `("created",)` | — | first composition of a new scope |
| `("state", …)` | the cell, the operation, the value before and after, where the cell was declared and the name it was bound to, where the write happened, the writer's thread, whether the write was posted, and how many readers it notified | a cell the scope read changed |
| `("derived", …)` | the derived, its declaration and name, its old and new value, whether it changed, and the cause that made it recompute | a derived the scope read recomputed to a different value |
| `("invalidated",)` | — | `Runtime.invalidate` was called with no cause: a manual invalidation |
| `("args", parameter, old, new, compared)` | the parameter and both values themselves; `compared` is False when the argument's type is unstable and was never compared | the parent ran and this argument differed |
| `("inline",)` | — | the scope takes a content block, so it re-runs whenever its parent runs |
| `("recovery", error)` | the previous run's error, as its `str` | the previous run raised |
| `("uncommitted",)` | — | the scope's last fragment never reached the core: it was created in a frame whose commit did not happen, or it ran under a parent whose run then raised and the rollback discarded what it emitted — so it runs again when next reached instead of referencing a fragment the core never received |
| `("dirty", inner_causes)` | the causes that made it dirty | the scope was already dirty when its parent reached it |

the state cause in full — the same tuple is the `cause` of a write record:

```text
("state", cell_id, cell_kind, op, at, old, new, decl_file, decl_line, decl_name,
    write_file, write_line, thread, posted, readers)
```

`cell_kind` is `state`, `list`, `dict` or `ambient`; `op` is `set`, `append`, `insert`, `remove`,
`pop`, `clear`, `put`, `delete` or `provide`; `at` is the index or key the op touched, `None` for a
whole-cell write, and a dict key that is neither an `int` nor a `str` is written as its `repr` so the
slot never holds a program object. a key change has no cause of its own: whether an old key was
really given up is known only when the parent's run ends, so the new scope says `created` and the
parent's `disposed` names the old key. `decl_name` is the name the cell was bound to (`count` for
`let count = state(0)`), recovered from the bytecode after the call site; it and the declaration
site are `None` for a cell created outside composition. `readers` is how many trackers the write
notified — `0` says nothing depended on the cell

a scope popped from the heap carries every state and derived cause recorded for it since its last
run, in the order they happened. the runtime records a cause *before* the dirty de-duplication, so
a handler that writes three cells produces one recomposition and three causes on it: batching still
means one run, and the record still says all three reasons

`old` and `new` — of a state cause, of an args cause and of a derived cause — are the slots of a
record that are whatever the program stored, not builtins. an in-process reader may `repr` them; the
debugger never does — it renders them by exact type, and never runs program code to read a record.
a cell created outside composition (a module-level `State(0)`) binds to the runtime that first reads
it, and from then on its writes are recorded like any other cell's, with no declaration site

## a skipped scope is named, not implied

when a parent re-runs, each child it reaches is either run or skipped, and a skipped child is
emitted as a reference to its retained subtree instead. the parent's run record names them:
`skipped` is the tuple of scope ids the run emitted as references, and `disposed` the
`(id, name, key)` of every child disposed after the run because the run did not reach it

so "why did `Row` not re-run" has the same kind of answer as "why did it": it is in its parent's
record, by id, and not inferred from the absence of a record of its own. ids are stable for the
life of a scope and a disposed id is never reused by the same runtime, so a scope that was skipped
in one frame and ran in the next is the same id in both records

## the other records

| kind | shape | when |
| --- | --- | --- |
| write | `(2, frame, cause)` | one per public mutator call that changed something, and one per derived recompute — including a recompute whose value compared equal, with `changed` False. a write with `readers == 0` is the record a reader uses to say "nothing depends on this" |
| frame | `(3, frame, runs, skips, compose_ns, commit_ns)` | at the end of every `Runtime.frame` that ran at least one scope |
| error | `(4, frame, scope_id, name, error, kept_previous)` | a scope raised; `kept_previous` is whether a committed subtree stayed on screen |
| refused | `(5, frame, scope_id, name, what)` | a write during composition was refused; `what` names the cell kind and op, and the `CompositionError` is raised as before |

records are appended in the order things happened on the ui thread. within one frame, write records
precede the run records they caused and the frame record is last. a write from another thread is
recorded when it is applied at the next frame, with `posted` True and the writer's own thread and
site

## the bounds, and the dropped count

`Trace.limit` defaults to 4096 records. when `records` reaches it, the oldest `limit // 2` are
deleted and `Trace.dropped` grows by that much — so `dropped` is exactly how many records are gone,
ever, and a reader that shows the ring shows that count beside it rather than a ring that looks
complete. `Trace.frames` is the runtime's frame counter at the last append, so a reader can tell a
quiet program from one whose records all fell off

## it is on by default, and it costs a tuple

tracing is on unless turned off: `run_app(..., trace=True)`, `compose_test(..., trace=True)`,
`Runtime(core, trace=True)`. `trace=False` sets `Runtime.trace` to `None`, and the runtime then
pays one attribute test per scope run and per state write

every record appended is also announced through `sys.audit("basedpython_ui.trace", record)`, on the
ui thread. with no audit hook installed that costs a tuple and a builtin call; `bpd` hooks it
natively and forwards records only while a client is watching. every runtime registers itself in
`basedpython_ui.runtime.live_runtimes` when it is created and leaves on `dispose()`, which is how a
debugger finds the ring without the program's help

## the python api

| call | gives |
| --- | --- |
| `compose_test(root)`, `compose_test(trace=True): …` | a headless composition with tracing on, which is the default |
| `run_app(title, trace=True): …` | the same for a window |
| `Runtime.trace` | the `Trace`: `records` (oldest first), `dropped`, `limit`, `frames` — or `None` when tracing is off |
| `Runtime.explain(since_frame=None)` | the ring rendered as text — one entry per record, causes indented under their run, oldest first — with every location mapped to its `.by` line through `_by_sourcemap` when that module is importable and its digests still match, and left as the generated `.py` location otherwise |
| `TestComposition.why(fn)` | the run records of the scopes of composable `fn`, matched by identity (never by name), as a tuple of `Recomposition` frozen data classes: `frame`, `scope`, `parent`, `name`, `origin`, `key`, `defined`, `called`, `causes` (a tuple of `Cause`: `kind`, `name`, `old`, `new`, `file`, `line`, `posted`, `changed`, `compared`, `error`, `inner`), `skipped`, `disposed`, `elapsed_ns`, the raw tuple, and `.text()` |
| `TestComposition.explain(since_frame=None)` | `Runtime.explain` for the harness's runtime |

the records themselves are the exact answer, and a test asserts on them: the causes of a run are a
tuple of tuples, so `t.runtime.trace.records` is what to compare against when the *reason* matters
and `t.recompositions(Counter)` is enough when only the count does

```by
from basedpython_ui import composable, state, Column, Text, Button, compose_test


@composable
def Counter(step: int = 1):
    let count = state(0)
    Column:
        Text(f"count = {count.value}")
        Button("+"):
            count.value += step


@composable
def App():
    Column:
        Counter()
        Counter(step=5)


def test_the_second_counter_is_not_re_run_by_the_first():
    let t = compose_test:
        App()
    t.click("+")                                # the first counter's button
    t.advance()
    assert t.recompositions(Counter) == 3       # two initial compositions, one recomposition
    let runs = t.why(Counter)
    assert [r.origin for r in runs] == ["first", "first", "self"]
    let cause = runs[2].causes[0]
    assert (cause.kind, cause.name, cause.old, cause.new) == ("state", "count", 0, 1)
    print(t.explain())
```

what `explain()` prints for the counter example (`examples/counter.by`, whose `App` composes the
same two counters), captured after one click on the first `+`; a path under the working directory
is shown relative to it:

```text
frame 0: Counter#3 ran (first composition) in 97.2 µs, called at examples/counter.by:23
    created: first composition
frame 0: Counter#4 ran (first composition) in 35.4 µs, called at examples/counter.by:24
    created: first composition
frame 0: App#2 ran (first composition) in 7186.5 µs, called at capture.py:3
    created: first composition
frame 0: root#0 ran (first composition) in 7210.2 µs
    created: first composition
frame 0: 4 runs, 0 skipped, compose 7227.3 µs, commit 6494.1 µs
frame 1: write: count (state) set 0 → 1 at examples/counter.by:13 — 1 reader invalidated
frame 1: Counter#3 ran (from the dirty heap) in 52.3 µs, called at examples/counter.by:23
    count (state) set 0 → 1 at examples/counter.by:13
frame 1: 1 run, 0 skipped, compose 70.7 µs, commit 58.5 µs
```

and `why(Counter)` is the three run records of that transcript: two `first` runs whose cause is
`created`, called from lines 23 and 24, and one `self` run whose cause is the state write of `count`
from `0` to `1` at line 13 — the handler's own line, which is where the debugger stops too

there is no record for `Counter#3` in frame 1, and there is no record of `App` either: nothing it
read changed, so it was not dirty and never ran — which is why its children were not reached, not
skipped. had `App` re-run, its record would list `3` under `skipped`

## over DAP and MCP, at parity

the debugger reads the same ring. `bpd/recompositions` over DAP and the `recompositions` tool over
MCP return the records of every live runtime — each as json, with every location mapped through the
build's source map to its `.by` line, and `old` / `new` rendered by exact type — beside `format`,
`tracing`, the number of runtimes, and `dropped`. `bpd/watchRecompositions` and
`watch_recompositions` turn forwarding on: DAP pushes a `bpd/recomposition` event per record when
the client has named it in `bpd/understands`, and narrates one console line per run record
otherwise; MCP, which has no push, hands the records back under a `recompositions` key on the next
answer, at most 200 between calls and the rest counted in `dropped`

both front ends reach both requests, and both carry what the debugger says unasked; the difference
between them is carriage — pushed or pulled — and nothing here is a facet one of them cannot carry.
the answer, the refusals and what pycharm makes of it are on [the debugger](debugger.md)

## in the editor

the editor has two views of the same question, and they are different instruments. the compiler's
[`inferredInvalidations` hint](inlay-hints.md#the-write-side-inferredinvalidations) sits after a
write and says what it *may* re-run — static, a superset, known before the program runs. the trace
says what a write *did* re-run, with the value before and after and the frame it happened in. at a
stop, pycharm shows the record on the composable it reached: a label on the header line with the
latest run's cause, and the *basedpython Recompositions* tool window with every frame, scope and
cause of the session and the write site a click away

## what is not built

- the ring is not persisted. it lives in the process, `dropped` says what fell off, and `bpd` is
    how records leave it
- nothing judges. there is no storm detection and no threshold on `elapsed_ns` or on how often a
    scope ran — a record is a fact, and how many runs is too many is the reader's call
- the trace stops at commit. a frame record carries `compose_ns` and `commit_ns`; layout and paint
    time are the core's `last_frame_ms`, not attributed to a scope
- the runtime never maps to `.by` lines itself. `explain()` maps when `_by_sourcemap` is importable
    and `bpd` maps through the build's source map; a raw record carries the generated `.py` path
- a posted write is recorded when it is applied on the ui thread, not when the other thread made
    it. between the two nothing is visible, and the record's `thread` and site are the writer's
