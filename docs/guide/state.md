# state and mutation safety

a composable reads cells while it composes and is re-run when one of them changes. that only works
if every value a composition depends on either cannot change or notifies when it does — and this
page is that one rule, from the cells that make it easy to hold, through where the checker enforces
it, to the guards that hold when the checker cannot see

## the cells

| call | gives | notifies its readers on |
| --- | --- | --- |
| `state(initial)` | a `State[T]`: one stable value, read and written through `.value`. `set(new)` and `update(fn)` exist for lambdas, which cannot contain an assignment | a write whose new value is not equal to the old one |
| `state_list(initial=())` | a `StateList[T]`: an observable sequence of stable items — `len`, iteration, `[i]`, `[i] = v`, `append`, `insert`, `remove_at`, `remove`, `pop`, `clear`, `index_where`, `snapshot`; `each(key=…):` and `each_indexed(key=…):` are the keyed loops | every mutator |
| `state_dict()` | a `StateDict[K, V]`: an observable mapping — `len`, `in`, `[k]`, `[k] = v`, `get`, `remove`, `keys`, `items` | every mutator |
| `derived(compute)` | a `Derived[T]`: a value computed from other cells, recomputed when one of them changes, read through `.value` | a recompute whose result differs from the last |
| `remember(compute)` | the value `compute` returned the first time, for the scope's lifetime. not observable: it is a cache, not a cell | never |

every one of them is a **slot**: memoised by its position inside the scope that created it, so
`let count = state(0)` on the scope's second run returns the cell its first run made rather than a
new one. `let` is what holds a cell — the binding can never be rebound, only the cell's contents
change, and every change notifies

```by
frozen data class Todo:            # immutable: the only way to change a todo is to replace it in the list
    id: int
    title: str
    done: bool = False


@composable
def TodoApp():
    let todos = state_list([Todo(1, "write the runtime"), Todo(2, "ship it")])   # StateList[Todo]: observable
    let draft = state("")
    let next_id = state(3)
    let remaining = derived(lambda: sum(1 for t in todos if not t.done))        # ⟨depends on todos⟩

    Column(Modifier().padding(12)):
        Text(f"{remaining.value} of {len(todos)} remaining")
        Row:
            TextField(draft.value, placeholder="what needs doing?", modifier=weight(1.0)):
                draft.value = it                                                # `it: str`, the new text
            Button("add", enabled=draft.value != ""):
                todos.append(Todo(next_id.value, draft.value))
                next_id.value += 1
                draft.value = ""
        todos.each_indexed(key=lambda todo: todo.id):                          # keyed children; `it` is this row
            let row = it                                                        # the inner block below has its own `it`
            TodoRow(row.item, on_delete=lambda: todos.remove_at(row.index)):
                todos[row.index] = Todo(row.item.id, row.item.title, it)        # `it: bool` from `on_toggle`
        if len(todos) == 0:
            Text("nothing to do")
```

`todos.each_indexed(key=…):` is the keyed loop: a method taking a `local` block whose parameter is
the item, so a handler written inside captures *this* item. the obvious spelling —
`for todo in todos: key(todo.id): Button("x"): todos.remove(todo)` — closes over the loop variable,
so every button would delete the last todo; the fork makes that spelling a compile error and the
api makes it unnecessary

## what may be held in state

a type is **stable** — deeply immutable — when it is: `int`, `float`, `bool`, `str`, `bytes`,
`None`; a `tuple` / `frozenset` of stable elements; a `frozen data class` whose fields are stable;
an `enum class` member (payload variants lower to frozen data classes); a framework observable
(`State`, `StateList`, `StateDict`, `Derived`, `Ambient` — identity-stable handles whose mutations
notify); a callable (stable by identity). everything else — `list`, `dict`, `set`, `bytearray`, a
plain `data class`, an ordinary class with writable attributes, `object`, `dynamic` — is unstable

only a stable value may enter a cell: the initial value of `state`, the elements of a
`state_list`, the values of a `state_dict`, the result of a `derived` or `remember`. a `list` in a
`State` would be a value that changes without telling anyone, which is exactly the thing the
framework exists to make impossible. the checker reports it as `mutable-state-value` at the point
of entry, and the runtime checks the same thing on every write, for the callers the checker cannot
see

## the guarantee lives on the read side

a mutation of non-observable data is never a *trigger*: an immutable value cannot change, an
observable notifies when it does, a mutable value changes without telling anyone. so the sound
rule is that **a composition may only depend on immutable or observable values** — a parameter, a
global or a captured name read during composition must be one or the other

once that holds it does not matter where a write happens: another file, a `.py` module, a
`dynamic` value, a callback. either the value cannot change, or the change notifies. the write-side
rule (`silent-mutation`) stays because it points at the exact line; the read-side rule
(`unobservable-dependency`) is what makes the property general. both are on
[the compiler's rules](lints.md)

## slow, but never stale

the runtime invariant that holds regardless of what the checker saw:

> a composable is skipped only when every argument is provably stable and structurally equal;
> any other argument disables skipping for that scope

so a mutable value can only render stale if it is mutated *and no state changes afterwards*. the
runtime is "slow but never stale" for unstable arguments, and the lints turn the slow cases into
errors or warnings — `unstable-parameter` is the warning that says a scope is correct and never
skipped, and the `⟨unstable ⟩` [inlay hint](inlay-hints.md) is the same fact in the editor

## where a write may happen

composition is not the place to write: a write to a cell from a composable's body — or from a
`once` content block, which runs as part of the body — is `state-write-in-composition` at compile
time and a `CompositionError` at runtime, raised before anything is mutated. the places a write
belongs are the ones that run *later*: a handler block (`Button("+"): count.value += step`), a
lambda, a nested `def`, and an effect block

writes are batched. invalidation only marks the scopes that read the cell dirty and nothing runs
synchronously, so a handler that writes ten cells causes one recomposition, and a handler sees its
own writes. a `Derived` recomputes at most once per change of its inputs and notifies only if its
value changed, and composition reads deriveds after all of a frame's writes are applied — a scope
never observes one derived stale next to another fresh. a write from a thread other than the ui
thread is posted and applied at the next frame, never applied in place

## the runtime guards, always on

each of these is cheap, and each is the defence for a caller the checker cannot see — a `.py`
module, a `dynamic` value:

- writes to `State.value` / `StateList` / `StateDict` while the composer is composing raise
    `CompositionError` before mutating anything (one int compare per write)
- a value entering state is checked against a per-type cache of forbidden classes (`list`, `dict`,
    `set`, `bytearray`, non-frozen data classes) and refused with a `TypeError`
- a slot kind or order mismatch on recomposition raises with the composable's path
- writes from a thread other than the ui thread are posted, never applied in place
- a `Derived` that reads itself raises
- a user exception during composition discards that scope's output, keeps the previously committed
    subtree, reports through `on_error`, and never touches the retained tree

## honest holes

| hole | mitigation |
| --- | --- |
| mutation through a callee in another file (`helper(items)` appends) | closed by `unobservable-dependency`: a composition cannot read a mutable parameter or global in the first place, so the helper's write has nothing to make stale. an interprocedural `mutates` fact would only improve the message |
| `.py` callers and `dynamic` | state entry is guarded at runtime; a `dynamic`-typed read in composition is exempt from the read-side rule (gradual typing is exempt everywhere) |
| a `derived` / `remember` imported under another name, or a nested `def` passed to one | not recognised as composition-time code by the read-side rule (detection is by name); the composable body around it is still checked |
| a module attribute read during composition (`math.tau`) | exempt: a module is treated as a namespace, so a mutable module-level value is only checked when read through its own name (a global) |
| a frozen data class holding a mutable field typed `object` | rejected statically by `mutable-state-value`; the runtime guard checks declared field types one level down |
| a fresh lambda passed every composition | correct but never skipped; `remember` a handler on hot paths |

what a write did at runtime — which scopes it re-ran, with the value before and after — is
[why did this rerender](why-did-this-rerender.md)
