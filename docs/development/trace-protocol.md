# the trace protocol (`basedpython_ui.runtime.Trace`)

the runtime keeps a bounded record of why every scope ran, what every state write did, and what
every frame cost. three consumers read it, and this page is the contract between them:

- the runtime's own `Runtime.explain()` / `TestComposition.why(fn)` (python, in process)
- the debugger `bpd`: `bpd/recompositions` over DAP and `recompositions` over MCP read the ring at a
    stop; `bpd/watchRecompositions` / `watch_recompositions` stream records while the program runs
- the pycharm plugin, through bpd

every value in a record is an exact builtin (`int`, `str`, `bool`, `float`, `None`, `tuple`) except
the value slots — `old` and `new` of a state cause, of an args cause and of a derived cause — which
are whatever the program stored. a reader that must not run program code renders those by exact
type (a builtin scalar as itself, a container as `list[3]`, anything else as `a Todo`) and never
calls `repr` on them. no other slot ever holds a program object: the runtime renders a dict key
that is not an `int` or a `str` before it writes the `at` slot, and refuses any `key(...)` value
that is not an `int` or a `str` (a `bool` included)

## format version

```by
TRACE_FORMAT = 1          # module level, basedpython_ui.runtime
```

a reader compares this before reading anything else and refuses a mismatch by name. a change to any
record layout below bumps it

## where the ring lives

```by
live_runtimes: list[Runtime]      # module level; every Runtime appends itself in init and removes
                                  # itself in dispose(); a debugger finds runtimes here
Runtime.trace: Trace?             # None when tracing is off for this runtime
Trace.records: list[tuple]        # oldest first
Trace.dropped: int                # records that fell off the front, ever
Trace.limit: int                  # when len(records) reaches limit, the oldest limit // 2 are
                                  # deleted and dropped grows by limit // 2
Trace.frames: int                 # the runtime's frame counter at the last append
```

`Runtime` and `Trace` keep ordinary instance dictionaries (no `__slots__`), and `live_runtimes`,
`records` are exact `list`s: the debugger reads them through storage, never through attribute access
that could run program code

tracing is on by default (`Runtime(core, trace=True)`, `run_app(..., trace=True)`,
`compose_test(..., trace=True)`); `trace=False` sets `Runtime.trace = None` and the runtime pays one
attribute test per scope run and per state write

## the audit event

every record appended is also announced: `sys.audit("basedpython_ui.trace", record)` with the record
tuple as the one argument, on the thread that appended it (always the ui thread), after the append.
with no audit hook installed this costs a tuple and a builtin call; `bpd` hooks it natively and
forwards records only while a client is watching, and it never blocks the ui thread to do so: a
record it cannot hand on at once is dropped and counted, and the count travels with the next record

## records

the first element of every record is its kind

### run — a scope ran

```text
(1, frame, scope_id, parent_id, name, def_file, def_line, call_file, call_line, key,
    origin, causes, skipped, disposed, elapsed_ns)
```

| slot | type | meaning |
| --- | --- | --- |
| `frame` | int | `Runtime.frames` when the run started |
| `scope_id` | int | `Scope.id`; the root is 0 |
| `parent_id` | int or None | the parent scope's id; None for the root |
| `name` | str | the composable's `__name__` (`root` for the root scope) |
| `def_file`, `def_line` | str, int | `fn.__code__.co_filename` and `co_firstlineno` of the composable |
| `call_file`, `call_line` | str or None, int or None | the call site in the parent (the caller frame's code and the line of its `f_lasti`); None for the root |
| `key` | int, str or None | the enclosing `key(...)` value; `key()` refuses every other type, `bool` included |
| `origin` | str | `"first"` (created this run), `"self"` (popped from the dirty heap), `"parent"` (its parent ran it) |
| `causes` | tuple of cause tuples | why it ran (below); never empty |
| `skipped` | tuple of int | ids of child scopes this run emitted as references instead of running |
| `disposed` | tuple of `(id, name, key)` | children disposed after this run because the run did not reach them |
| `elapsed_ns` | int | wall time of the body, `perf_counter_ns` |

file paths are the generated `.py` paths the interpreter runs; the runtime never maps them to `.by`
lines. `bpd` maps every location through the build's source map, and `Runtime.explain()` maps through
`_by_sourcemap` when it is importable

### causes

| cause | when |
| --- | --- |
| `("created",)` | first composition of a new scope |
| `("state", ...)` | a cell the scope read changed — the full tuple is the state cause below |
| `("derived", ...)` | a derived the scope read recomputed to a different value — the derived cause below |
| `("invalidated",)` | `Runtime.invalidate` was called with no cause (a manual invalidation) |
| `("args", parameter, old, new, compared)` | the parent ran and this argument differed; `old` and `new` are the argument values themselves; `compared` is False when the argument's type is unstable and was never compared |
| `("inline",)` | the scope takes a content block, so it re-runs whenever its parent runs |
| `("recovery", error)` | the previous run raised `error` (its `str`) |
| `("uncommitted",)` | the scope's last fragment never reached the core: it was created in a frame whose commit did not happen, or it ran under a parent whose run then raised, so the rollback discarded what it emitted and the next reach runs it instead of referencing a fragment the core never received |
| `("dirty", inner_causes)` | the scope was already dirty when its parent reached it; `inner_causes` is the tuple of the causes that made it dirty |

a key change has no cause of its own, because whether an old key was really given up is known only
when its parent's run ends: the new scope carries `("created",)`, and the parent's run record names
the old key under `disposed`

a scope popped from the heap (`origin == "self"`) carries every state and derived cause recorded for
it since its last run, in the order they happened; the runtime records a cause before the dirty
de-duplication, so a handler that writes three cells produces three causes on one run

### state cause

```text
("state", cell_id, cell_kind, op, at, old, new, decl_file, decl_line, decl_name,
    write_file, write_line, thread, posted, readers)
```

| slot | type | meaning |
| --- | --- | --- |
| `cell_id` | int | `id()` of the cell |
| `cell_kind` | str | `"state"`, `"list"`, `"dict"`, `"ambient"` |
| `op` | str | `"set"`, `"append"`, `"insert"`, `"remove"`, `"pop"`, `"clear"`, `"put"`, `"delete"`, `"provide"` |
| `at` | int, str or None | the index or key the op touched; None for a whole-cell write. a dict key of any other type is written as its `repr`, cut at 64 characters, so the slot never holds a program object |
| `old`, `new` | any | the value before and after (for `append`: None and the item; for `clear`: the length and 0) |
| `decl_file`, `decl_line`, `decl_name` | str or None, int or None, str or None | where the cell was created (`state(...)` call site) and the name it was bound to; all None for a cell created outside composition |
| `write_file`, `write_line` | str, int | the frame that called the public mutator |
| `thread` | int | `threading.get_ident()` of the writer |
| `posted` | bool | the write came from another thread and was applied at the next frame |
| `readers` | int | trackers notified; 0 means nothing depended on the cell |

`decl_name` is recovered from the bytecode after the slot's call site (`STORE_FAST` / `STORE_DEREF`)
the first time a site is seen and cached per site; it is None when no store follows the call

a cell created outside composition (a module-level `State(0)`) has no runtime when it is made; it
binds to the runtime whose tracker first reads it, and from then on its writes are recorded, posted
and refused like any other cell's, with `decl_file`, `decl_line` and `decl_name` None

### derived cause

```text
("derived", derived_id, decl_file, decl_line, decl_name, old, new, changed, because)
```

`old` and `new` are the derived's values themselves. `because` is the state or derived cause that made it recompute. `changed` False means the value
compared equal and no reader was invalidated; such a recompute is still recorded as a write record

### write — a state write happened

```text
(2, frame, cause)
```

one per public mutator call that changed something (`cause` is a state cause), and one per derived
recompute (a derived cause). a write with `readers == 0` is the record a reader uses to say "nothing
depends on this"

### frame — a frame finished

```text
(3, frame, runs, skips, compose_ns, commit_ns)
```

appended at the end of every `Runtime.frame` that ran at least one scope

### error — a scope raised

```text
(4, frame, scope_id, name, error, kept_previous)
```

`error` is `str(exception)`; `kept_previous` is whether a committed subtree stayed on screen

### refused — a write during composition was refused

```text
(5, frame, scope_id, name, what)
```

`what` names the cell kind and op that was refused; the `CompositionError` is raised as before

## what a reader may assume

- records are appended in the order things happened on the ui thread; a posted write's record is
    appended when it is applied, with `posted` True and the writer's own thread and site
- within one frame, write records precede the run records they caused, and the frame record is last
- ids are stable for the life of a scope; a disposed id is never reused by the same runtime
- `limit` defaults to 4096 records; `dropped` says exactly how many are gone
