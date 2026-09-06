# the guide

each page here is the user's side of one thing the framework does. the decisions behind them are
on the [design](../development/design.md) page, and the two wire-level contracts are the
[native core protocol](../development/native-protocol.md) between the basedpython runtime and the
rust core, and the [trace protocol](../development/trace-protocol.md) between the runtime, the
debugger and the editor

[state and mutation safety](state.md) is the rule everything else rests on: a composition may only
depend on values that cannot change or that notify when they do. the page covers the cells —
`state`, `state_list`, `state_dict`, `derived`, `remember` — what may be held in them, why the
guarantee is enforced where a value is *read* rather than where it is written, the runtime
invariant that makes an unstable argument slow rather than stale, the guards that are always on,
and the holes that remain, named

[the compiler's rules](lints.md) is the table of lints the fork adds under `by check`, and then a
file of deliberate mistakes with the verbatim diagnostics it produces — so what each rule catches,
and what it deliberately leaves alone, can be read off real output rather than described

[inlay hints](inlay-hints.md) is what the editor shows: the observables a composable reads, what a
`derived` depends on, which parameters are unstable, and — after a state write — which composables
the write invalidates. it explains the notation the docs use for a hint, the settings that switch
each one off, and why the read set is a superset while the runtime's answer is exact

[why did this rerender](why-did-this-rerender.md) is the runtime's own account of every scope run:
a bounded ring of records carrying the scope, the frame, the cause — a state write with its old and
new value and its site, an argument that differed, a structural reason — the scopes it skipped, and
what the body cost. reachable in process as `Runtime.explain()` and `TestComposition.why(fn)`, and
through the debugger

[the debugger](debugger.md) is `bpd`'s surface for that ring as a user sees it: the two custom
requests and the tools, the event and the `bpd/understands` handshake, the json of an answer, what
is refused and why, what pycharm shows in its tool window and in the editor margin, and how to turn
tracing off at each level

[testing](testing.md) is `compose_test`: the same runtime and the same native layout with no
window, `find_text` / `click` / `type_into` / `advance`, `recompositions` and `why`, the difference
between the fake core the runtime tests use and the real one the harness uses, and the commands
that run everything, including the free-threaded run
