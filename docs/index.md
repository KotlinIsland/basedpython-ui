# basedpython-ui

a compose / swiftui / flutter inspired ui framework for basedpython. a `@composable` function
describes a piece of ui by emitting into a composer, and re-runs when one of the observables it
read changes — exactly those, and nothing else

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
    (`def Counter(step: int = 1)⟨ reads count⟩:`), what each `derived` depends on, which
    parameters are `⟨unstable ⟩`, and what a state write `⟨ invalidates Counter⟩` — inferred by
    the compiler fork, rendered by `by server`; and at runtime every recomposition is recorded with
    its cause, so "why did this rerender" has an exact answer
- **fast where it matters**: composition and state in basedpython, layout / text / paint / input in
    a rust core, one batched commit per frame

## what it takes from each

- composables are plain functions that *emit* into a composer; state is an
    observable cell whose reads are tracked and whose writes invalidate exactly the scopes that read
    it; positional memoization with `key()`; skipping of a composable whose arguments are stable and
    equal; `remember` / `derived` / effects; `Modifier` chains; scope-restricted modifiers (`weight`
    only inside a `Row:`); ambient values
- value semantics for everything held in state — only frozen data classes,
    scalars, tuples and enum variants may sit inside a `State`. this is what turns "no silent
    mutation" from a convention into a type property
- a three-tree pipeline (a per-frame configuration, a retained element tree,
    render objects that lay out and paint) with constraints-down / sizes-up layout, relayout and
    repaint boundaries, hit testing on the render tree, and a headless test harness as a first-class
    deliverable

## what it rejects

- compose's compiler plugin. basedpython already has trailing blocks, implicit receivers, `once`,
    `context` parameters and a type checker with an extension surface; the few things a plugin adds
    (recognising composables, stability, read sets) become checker facts in the fork, never a rewrite
    of user code
- retained content lambdas. a framework that stores a `once` block and re-invokes it later lies to
    the checker; content blocks run exactly once per execution of the body that wrote them, and a
    composable that takes a block re-runs with its parent
- per-node python objects. a node is eight ints in a flat buffer; the element and render trees live
    in rust
- "mutate a list, maybe it re-renders". a plain `list` cannot enter state; mutating a non-observable
    container inside a composable is a compile error; writing state *during* composition is a compile
    error and a runtime guard

## where to go

<div class="grid cards" markdown>

- :lucide-book-open:{ .lg .middle } **[the guide](guide/index.md)**

    ______________________________________________________________________

    one page per thing the framework does, from the user's side: state, the compiler's rules,
    the editor, the runtime's own account of every re-render, the debugger, and the test harness

- :lucide-shield-check:{ .lg .middle } **[state and mutation safety](guide/state.md)**

    ______________________________________________________________________

    the cells, what may be held in them, why the guarantee lives on the read side, and the
    guards that hold when the checker cannot see

- :lucide-triangle-alert:{ .lg .middle } **[the compiler's rules](guide/lints.md)**

    ______________________________________________________________________

    every lint the fork adds, and what `by check` says about a file of deliberate mistakes

- :lucide-search:{ .lg .middle } **[why did this rerender](guide/why-did-this-rerender.md)**

    ______________________________________________________________________

    the record the runtime keeps of every scope run, its cause, and what it skipped — in
    process, in the debugger, and in the editor

- :lucide-bug:{ .lg .middle } **[the debugger](guide/debugger.md)**

    ______________________________________________________________________

    `bpd/recompositions` over DAP, `recompositions` over MCP, what is refused, and what pycharm
    shows at a stop

- :lucide-blocks:{ .lg .middle } **[development](development/design.md)**

    ______________________________________________________________________

    the design as it was decided, the native core protocol, and the trace protocol the runtime,
    the debugger and the editor agree on

</div>
