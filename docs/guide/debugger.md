# the debugger

`bpd` reads the runtime's trace ring and hands it to a client — over DAP to an editor, over MCP to
an agent. this page is that surface as a user sees it: what to send, what comes back, what is
refused and why, and what pycharm makes of it. what a record *means* is
[why did this rerender](why-did-this-rerender.md); the layout the runtime writes is the
[trace protocol](../development/trace-protocol.md)

## two requests

| ask | DAP | MCP |
| --- | --- | --- |
| the ring, at a stop | custom request `bpd/recompositions {}` | tool `recompositions {session?}` |
| forward records while the program runs | custom request `bpd/watchRecompositions {on}` → `{watching: bool}` | tool `watch_recompositions {on, session?}` |

both are about the program, not a frame, so any held thread answers them. `session` names the
session a call is for when more than one is open; with one open it is not needed, and with two a
call naming none is refused with the list, the way every session-scoped tool behaves

## the answer

```json
{
  "format": 1,
  "runtimes": 1,
  "tracing": true,
  "records": { "kept": [ ], "dropped": 0 },
  "mode": { }
}
```

| key | meaning |
| --- | --- |
| `format` | the runtime's `TRACE_FORMAT`. anything but `1` is refused by name, below |
| `runtimes` | how many runtimes `live_runtimes` held. the records of every runtime are concatenated in runtime order, each record carrying `runtime`, its index — a program normally has one |
| `tracing` | `false` when every runtime has `trace = None`; then `records.kept` is empty and the answer is the refusal `ui_tracing_off` instead |
| `records` | `kept`, oldest first, and `dropped`: the runtime's own `dropped` plus anything bpd left out — it caps at 4096 records per answer, newest kept |
| `mode` | the read mode every bpd read carries, serialised as every other read does |

over MCP the answer also carries `says`, one sentence naming the count and what fell off, the way
the `trail` tool does

## a record on the wire

```json
{ "record": "run", "runtime": 0, "frame": 3, "scope": 5, "parent": 0, "name": "Counter",
  "defined": { "file": "/app/examples/counter.by", "line": 9, "generated": { "file": "/tmp/build/examples/counter.py", "line": 41 } },
  "called": { "file": "/app/examples/counter.by", "line": 24, "generated": { "file": "...", "line": 90 } },
  "key": null,
  "origin": "self",
  "causes": [ ],
  "skipped": [7, 8],
  "disposed": [ { "scope": 9, "name": "Row", "key": 2 } ],
  "elapsed_ns": 12345 }

{ "record": "write", "runtime": 0, "frame": 3, "cause": { "cause": "state", "...": "..." } }

{ "record": "frame", "runtime": 0, "frame": 3, "runs": 2, "skips": 4, "compose_ns": 15000, "commit_ns": 300000 }

{ "record": "error", "runtime": 0, "frame": 3, "scope": 5, "name": "Flaky", "error": "boom", "kept_previous": true }

{ "record": "refused", "runtime": 0, "frame": 3, "scope": 5, "name": "Bad", "what": "state set" }
```

`called` is `null` for the root; `key` is `null`, an integer or a string. a location is the `.by`
file and line when the build's source map covers the generated file, with `generated` carrying the
location the interpreter actually ran; when there is no map, `file` / `line` *is* the generated
location and `generated` is `null`. it is the same mapping every frame bpd reports goes through,
and a generated line the map marks as having no source line keeps the generated location and says
so under `reason`, the way a frame does

```json
{ "file": "/app/examples/counter.by", "line": 24, "generated": { "file": "/tmp/build/examples/counter.py", "line": 90 } }
```

a cause is tagged the same way, with `cause`:

```json
{ "cause": "created" }
{ "cause": "invalidated" }
{ "cause": "inline" }
{ "cause": "uncommitted" }
{ "cause": "args", "parameter": "step", "old": "1", "new": "2", "compared": true }
{ "cause": "recovery", "error": "boom" }
{ "cause": "dirty", "causes": [ ] }
{ "cause": "state", "cell": 4401, "kind": "state", "op": "set", "at": null,
  "old": "0", "new": "2",
  "declared": { "file": "...", "line": 12, "generated": { } }, "declared_name": "count",
  "written": { "file": "...", "line": 14, "generated": { } },
  "thread": 8674, "posted": false, "readers": 1 }
{ "cause": "derived", "derived": 77, "declared": { }, "declared_name": "total",
  "old": "1", "new": "2", "changed": true, "because": { "cause": "state", "...": "..." } }
```

`old` and `new` are **rendered text**, never program objects: an exact builtin scalar as itself
(`"2"`, `"'abc'"`, `"None"`, `"True"`), an exact builtin container by kind and size (`"list[3]"`,
`"dict{2}"`, `"tuple[0]"`), and anything else as `"a Todo"` — the debugger's rule for every value
it shows without running program code, and text is cut at 64 characters with `…`. `declared` and
`declared_name` are `null` for a cell created outside composition; `at` is `null`, an integer or a
string (the runtime writes a dict key of any other type as its `repr`). there is no `key` cause: a
key change is `created` on the new scope plus the old key in the parent's `disposed`

## what it says unasked

while watching is on, every record the runtime appends is forwarded, and the two front ends carry
it the only way each can:

| front end | carriage |
| --- | --- |
| DAP | a custom event `bpd/recomposition` with body `{ "record": <record>, "dropped_before": N }` when the client named it in `bpd/understands`; otherwise one `output` line on the `console` category per **run** record — writes and frames are not narrated — and an `important` line whenever records were dropped |
| MCP | a `recompositions` key on the next answer, whatever the call: `{ "records": [<record>...], "dropped": N, "says": "..." }`, keeping at most 200 records between calls; `dropped` counts what the cap left out and what the program's outbound queue dropped, and `says` names which |

the stream never blocks the program. the record is rendered on the ui thread inside the audit
hook and handed to a bounded queue that a thread of bpd's own writes to the socket; when the queue
is full the oldest queued record is dropped and counted, and the count rides on the next record
that gets through as `dropped_before`. a client that stops reading for a while therefore sees a
gap with a number in it, never a frozen ui. a stop waits for the queue to drain first, so a record
written before a stop always arrives before it

`bpd/understands {"events": ["bpd/recomposition"]}` is how a DAP client says it reads the event, so
the console does not show the same record twice. pycharm names it; a client that has never heard of
the request keeps the prose, which is what makes the event an addition rather than a migration

## what is refused

a refusal is an answer with a name and a sentence, not an empty ring:

| refused | when | sentence |
| --- | --- | --- |
| `no_ui_runtime` | `basedpython_ui.runtime` is not in `sys.modules` | `the program has not imported basedpython_ui.runtime, so there is no trace to read` |
| `ui_tracing_off` | every runtime has `trace = None` | `tracing is off in the program's runtime; start it with trace=True` |
| `ui_trace_format { found, wanted }` | `TRACE_FORMAT != 1`, or the module has no `TRACE_FORMAT` (`found` is `null` then) | `the program's basedpython_ui writes trace format {found} and this bpd reads {wanted}`; with no `TRACE_FORMAT` at all, `… writes no trace format at all — basedpython_ui.runtime has no TRACE_FORMAT — and this bpd reads 1` |
| `ui_trace_unreadable { what }` | a record slot has a type the layout does not allow | `the trace record could not be read: {what}` |

a watch is accepted before the program has imported the runtime — watching is an interest in
records to come, and only the read at a stop needs the runtime to exist; a forked child starts with
it off. the debugger reads
`live_runtimes` and each runtime's `trace.records` through storage, never through an attribute
access that could run program code — which is why the runtime keeps them as plain instance
dictionaries and exact `list`s — and a record whose shape it does not recognise costs that record,
never the session

watching is a flag in bpd's agent, read by its native hook on the `basedpython_ui.trace` audit
event, so a program that nobody is watching pays for a tuple and a builtin call per record and
nothing more

## what pycharm shows

with the debug backend set to bpd, the plugin names `bpd/recomposition` in `bpd/understands` and
sends `bpd/watchRecompositions` as soon as the adapter is ready — before the program has run a
line, which bpd accepts — so records arrive as events while the program runs and never as console
prose; at every stop it asks `bpd/recompositions` and merges the answer, frame by frame, with what
the stream delivered

- **the tool window** — *basedpython Recompositions*, at the bottom: frame → run → causes, and
    under a run the scopes it skipped and disposed; a write that caused no run, a frame's cost, an
    error, a refused write and a gap in the stream (`dropped_before`) are rows of their own. *Jump
    to Source* opens the write site of a state cause or the definition of a composable, *Jump to
    Call Site* where the parent called it; both open the `.by` location and show the generated one
    in the tooltip, never opening it. *Watch* toggles the stream (the toggle shows what bpd last
    confirmed, and a refused watch is a notification once, never the window's state), *Refresh*
    reads the ring again while stopped, *Clear* forgets what is shown. the history is kept across
    the stops of one session, bounded at 8192 records with a note saying how many were let go, and
    cleared when the session ends; typing in the tree is a speed search
- **the margin label** — on a composable's definition line, after the line, at a stop: how many
    times that composable ran in the latest frame and the first cause, in the shape
    `ran ×2 · count 0 → 2, set at counter.by:13`. it is painted past the end of the line rather
    than inlaid, so nothing reflows, faded so it reads as an observation, and removed when the
    program resumes
- **the setting** — *Show why basedpython-ui composables ran*, in the Debugger group of the
    basedpython settings, on by default. off means the plugin neither asks at a stop nor watches,
    and the labels come off at once
- **when bpd cannot answer** — a refusal is shown with its sentence (`tracing is off in the
    program's runtime; start it with trace=True`), and a request that got no answer within two
    seconds says so rather than claiming nothing ran

the compiler's `inferredInvalidations` hint sits on the write in the same editor; the label is the
runtime's answer to it — [in the editor](why-did-this-rerender.md#in-the-editor)

## turning tracing off

| where | how | effect |
| --- | --- | --- |
| in the program | `run_app(..., trace=False)`, `compose_test(..., trace=False)`, `Runtime(core, trace=False)` | `Runtime.trace` is `None` and no record is ever appended; the runtime pays one attribute test per scope run and per write; bpd answers `ui_tracing_off` |
| in the debugger | `bpd/watchRecompositions {"on": false}`, `watch_recompositions {on: false}` | records keep accumulating in the ring and none is forwarded; `bpd/recompositions` still reads the ring at a stop |
| in pycharm | the *Show why basedpython-ui composables ran* setting | the plugin stops asking and watching and takes the labels off; the program is unchanged |
