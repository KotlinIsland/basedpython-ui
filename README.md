# basedpython-ui

a composeable reactive ui framework for
[basedpython](https://kotlinisland.github.io/basedpython/): a `@composable` function describes a
piece of ui, and re-runs when one of the observables it read changes

[read the docs](https://kotlinisland.github.io/basedpython-ui/)

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
  parameters are `⟨unstable ⟩` and what a state write `⟨ invalidates Counter⟩` — inferred by the
  compiler fork, rendered by `by server`; and at runtime every recomposition is recorded with its
  cause
- **fast where it matters**: composition and state in basedpython, layout / text / paint / input in
  a rust core, one batched commit per frame

the docs' guide on [testing](https://kotlinisland.github.io/basedpython-ui/guide/testing/) has the rest: the benchmark, the free-threaded run, and an example in a window
