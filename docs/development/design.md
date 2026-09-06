# basedpython-ui — design

*the user-facing parts of this document are carried by the guide — [state and mutation safety](../guide/state.md) for §3 and §4, [the compiler's rules](../guide/lints.md) for §4.3 and what used to be `docs/lints.md`, [inlay hints](../guide/inlay-hints.md) for §5, [testing](../guide/testing.md) for the headless test of §2 — and this page is kept as it was written*

*status: design settled 2026-09-02; the api surface in `src/basedpython_ui/` and every example in
`examples/` type-check with the real compiler and execute through the skeleton runtime. this
document records what was decided, why, and what remains to build.*

## 1. what it is

a ui framework written in basedpython that takes:

- from **compose**: composables are plain functions that *emit* into a composer; state is an
  observable cell whose reads are tracked and whose writes invalidate exactly the scopes that read
  it; positional memoization with `key()`; skipping of a composable whose arguments are stable and
  equal; `remember` / `derived` / effects; `Modifier` chains; scope-restricted modifiers (`weight`
  only inside a `Row:`); ambient values
- from **swiftui**: value semantics for everything held in state — only frozen data classes,
  scalars, tuples and enum variants may sit inside a `State`. this is what turns "no silent
  mutation" from a convention into a type property
- from **flutter**: the three-tree pipeline (a per-frame configuration, a retained element tree,
  render objects that lay out and paint) with constraints-down / sizes-up layout, relayout and
  repaint boundaries, hit testing on the render tree, and a headless test harness as a first-class
  deliverable

and rejects:

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

## 2. what the user writes

everything below is copied from `examples/`; each file passes `by check` and runs through the
skeleton runtime with `by build`.

### the counter

```by
from basedpython_ui import composable, state, Column, Row, Text, Button, Modifier, Alignment, run_app
from basedpython_ui.geometry import Dp


@composable
def Counter(step: int = 1):
    let count = state(0)                        # State[int], remembered for this scope's lifetime
    Column(Modifier().padding(16)):
        Text(f"count = {count.value}")          # a tracked read: this scope now depends on `count`
        Row:
            Button("-", enabled=count.value > 0):
                count.value -= step             # the block binds `on_click`; a write invalidates readers
            Button("+"):
                count.value += step
            Button("reset", enabled=count.value != 0):
                count.value = 0
        if count.value > 9:
            Text("that is a lot", modifier=align(Alignment.Center))   # `align` comes from the ColumnScope receiver


@composable
def App():
    Column:
        Counter()
        Counter(step=5)


def main():
    run_app("counter"):
        App()
```

what the language is doing here:

- `Column(...):` is a trailing-lambda block bound to `Column`'s last parameter,
  `once content: ColumnScope.() -> None`. inside it the receiver's members (`align`, `weight`)
  resolve unqualified, and the builders (`Text`, `Row`) resolve lexically
- `Button("+"):` binds the block to `on_click`. the block is *not* `once` (a handler runs many
  times), so the checker already forbids `return` inside it and forbids assigning an enclosing
  `let` from it; `count.value += step` is an attribute write on a captured handle, which is exactly
  what state mutation must always be
- `let count = state(0)` — `let` makes the binding final, so the cell can never be rebound; only its
  `.value` changes, and every change notifies

with the fork (section 5) the editor shows `def Counter(step: int = 1)⟨ reads count⟩:`.

### a todo list: keyed children and an observable list

```by
frozen data class Todo:            # immutable: the only way to change a todo is to replace it in the list
    id: int
    title: str
    done: bool = False


@composable
def TodoRow(todo: Todo, on_delete: () -> None, on_toggle: (bool) -> None):
    Row(Modifier().padding(4)):
        Checkbox(todo.done, on_change=on_toggle)
        Text(todo.title, modifier=weight(1.0))
        Button("x", on_click=on_delete)


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

`todos.each_indexed(key=…):` is the keyed loop. it is a method taking a `local` block whose
parameter is the item, so a handler written inside captures *this* item. the obvious spelling —
`for todo in todos: key(todo.id): Button("x"): todos.remove(todo)` — type-checks today and is
**wrong at runtime**: the handler closes over the loop variable and every button deletes the last
todo (verified: the lowering emits a plain closure with no per-iteration rebinding, and the
`escaping-loop-variable` check does not see through the `key` block). the fork makes that spelling
a compile error; the api makes it unnecessary.

`let row = it` names the row before the nested handler block, because that block has an `it` of its
own (the checkbox's new value). a nested block whose callback passes *no* argument used to declare
an `it` anyway, shadowing the outer one at runtime while the checker resolved it outward; the fork
fixes that lowering, so `Button("x"): todos.remove_at(it.index)` reads the row as written.

### a form with derived validation

```by
@composable
def SignupForm(on_submit: (Signup) -> None):
    let name = state("")
    let email = state("")
    let submitted = state(False)
    # derived values are recomputed only when a dependency changes, and never observed half-updated
    let name_error = derived(lambda: validate_name(name.value))
    let email_error = derived(lambda: validate_email(email.value))
    let can_submit = derived(lambda: name_error.value is None and email_error.value is None)

    Column(Modifier().padding(16)):
        Field("name", name.value, name_error.value if submitted.value else None):
            name.value = it
        Field("e-mail", email.value, email_error.value if submitted.value else None):
            email.value = it
        Row:
            Button("sign up", enabled=can_submit.value or not submitted.value):
                submitted.value = True
                if can_submit.value:
                    on_submit(Signup(name.value, email.value))
            Button("clear"):
                name.value = ""
                email.value = ""
                submitted.value = False
```

### theming: a static ambient and a runtime ambient

```by
# a static ambient: `context theme` is filled at every call site by the checker
# (the IDE shows `Toolbar(⟨theme=theme⟩)`); it costs nothing at runtime and a missing
# value is a compile error, not a runtime surprise
@composable
def PrimaryButton(label: str, on_click: () -> None, context theme: Theme):
    Button(label, modifier=Modifier().padding(theme.spacing).background(theme.primary), on_click=on_click)


@composable
def Toolbar(on_toggle: () -> None, context theme: Theme):
    Row(Modifier().background(theme.primary)):
        Text("themed app", color=theme.on_primary)
        PrimaryButton("toggle theme", on_click=on_toggle)          # `theme` forwarded implicitly


@composable
def Body():
    let scale = density.current                                     # runtime ambient read
    Text(f"density {scale}")


@composable
def App():
    let dark = state(False)
    context theme = DARK if dark.value else LIGHT                   # re-declared on every composition
    Column:
        Toolbar(on_toggle=lambda: dark.set(not dark.value))
        Body()
        provide(density, 2.0):                                      # overrides the ambient for this subtree
            Body()
```

two mechanisms on purpose. a `context` parameter is compile-time dependency injection: every
composable that transitively needs the theme says so in its signature, the checker fills every call
site, and forgetting to provide it is an error. a runtime `Ambient` is compose's `CompositionLocal`:
dynamically scoped over the tree, provided by an ancestor, read anywhere below, and its reads are
tracked. use `context` for values a component honestly depends on; use `Ambient` when a library
boundary makes threading impossible.

### async loading with cancellation

```by
@composable
def Profile(uid: int):
    let status = state(Load.Idle)
    # keyed on `uid`: a change cancels the running task and starts a new one; leaving the tree cancels it.
    # the block awaits, so it is a coroutine; `it` is the Job whose `is_active` flips on cancellation
    launched_effect(uid):
        status.value = Load.Loading                    # writes from an effect run between frames: fine
        try:
            let fetched = await fetch_user(uid)
            if it.is_active:
                status.value = Load.Ready(fetched)
        except ValueError as e:
            status.value = Load.Failed(str(e))
    disposable_effect(uid):
        it.on_dispose(lambda: print("profile", uid, "left the tree"))
    match status.value:                                # exhaustive over the enum
        case Load.Idle:
            Text("…")
        case Load.Loading:
            Text(f"loading user {uid}")
        case Load.Ready(user):
            Text(f"hello, {user.name}")
        case Load.Failed(reason):
            Text(f"error: {reason}", color="#cc0000")
```

### a custom layout and a canvas

```by
def flow_measure(scope: MeasureScope, constraints: Constraints) -> Size:
    """A custom layout policy: lay children out left to right, wrapping at the max width.
    Runs in the layout pass; constraints go down, sizes come back up."""
    let child = constraints.loose()
    var x = 0.0
    var y = 0.0
    var row_height = 0.0
    for i in range(scope.child_count):
        let size = scope.measure(i, child)
        if x > 0.0 and x + size.width > constraints.max_width:
            x = 0.0
            y += row_height
            row_height = 0.0
        scope.place(i, x, y)
        x += size.width
        row_height = max(row_height, size.height)
    return Size(constraints.max_width, y + row_height)


@composable
def FlowRow(labels: tuple[str, ...]):
    Layout(flow_measure, Modifier().fill_max_width()):
        keyed(labels).each(key=lambda s: s):
            Text(it, Modifier().padding(4))


@composable
def Clock(seconds: int):
    let angle = math.tau * (seconds % 60) / 60.0
    # the draw block runs at paint time against a DrawScope: `size` and the primitives resolve unqualified
    Canvas(Modifier().size(200, 200)):
        let cx = size.width / 2.0
        let cy = size.height / 2.0
        circle(cx, cy, 90.0, "#222222")
        line(cx, cy, cx + 80.0 * math.sin(angle), cy - 80.0 * math.cos(angle), "#ffffff", stroke=3)
```

### a headless test

```by
def test_counter_increments():
    let t = compose_test:                       # headless: same runtime, same native layout, no window
        Counter(step=2)
    assert t.find_text("count = 0")
    t.click("+")
    t.advance()                                 # run pending recompositions, commit, layout, effects
    assert t.find_text("count = 2")
    assert t.recompositions(Counter) == 2       # the initial composition and exactly one recomposition
```

## 3. the public surface

| area | names | rules |
|---|---|---|
| values | `Dp`, `Size`, `Offset`, `Rect`, `Constraints`, `Color`, `TextStyle`, `Alignment`, `Arrangement`, `ModOp`, `Modifier`, `NONE` | all `frozen data class` / `enum class`: deeply immutable, hashable, stable. `Dp.__of__(int \| float)` and `Color.__of__(str)` let literals stand in (`padding(8)`, `"#cc0000"`) because `.by` has no int→float promotion |
| state | `State[T]` / `state`, `StateList[T]` / `state_list`, `StateDict[K, V]` / `state_dict`, `Derived[T]` / `derived`, `remember` | slot-memoised by position inside a scope. `T` must be deeply immutable or observable. `State.set` / `update` exist for lambdas (a lambda cannot contain an assignment) |
| composition | `@composable`, `key(k):`, `keyed(items).each(key=…):`, `StateList.each` / `each_indexed` | the decorator is identity-typed (`[F](fn: F) -> F`), verified to keep the decorated name a function literal so `once`, keyword block binding and `context` parameters survive it |
| builders | `Text`, `Button`, `TextField`, `Checkbox`, `Spacer`, `Column`, `Row`, `Box`, `Canvas`, `Layout` | module-level functions (a class constructor cannot take a block). every content parameter is `once` and last; every handler is last so a block binds it. defaults are module-level `let` sentinels because a non-scalar default is re-evaluated per call |
| scopes | `ColumnScope`, `RowScope` (`weight`, `align`), `BoxScope` (`align`), `DrawScope`, `MeasureScope` | receivers of content blocks; they carry only what is legal inside that container |
| effects | `launched_effect(key): await …` (`it: Job`), `disposable_effect(key):` (`it: DisposeScope`), `side_effect` | run after commit, untracked; restarted on key change; cancelled on disposal |
| ambients | `context` parameters (static), `ambient` / `provide` / `.current` (runtime) | see the theming example |
| app | `run_app(title, width, height, root)`, `compose_test(root) -> TestComposition` | `run_app("x"): App()` — the block binds `root` |

parameter-order facts every signature obeys (all verified against the compiler): a trailing block
binds the callee's **last** parameter; a `context` parameter must be last too, so a composable
cannot yet take both a `context` parameter and a content block (fork item F3 relaxes this); a
required parameter may follow a defaulted one.

## 4. state, mutation safety and what is guaranteed

### 4.1 stability

a type is **stable** (deeply immutable) when it is: `int`, `float`, `bool`, `str`, `bytes`, `None`;
a `tuple`/`frozenset` of stable elements; a `frozen data class` whose fields are stable; an
`enum class` member (payload variants lower to frozen dataclasses); a framework observable
(`State`, `StateList`, `StateDict`, `Derived`, `Ambient` — identity-stable handles whose mutations
notify); a callable (stable by identity). everything else — `list`, `dict`, `set`, `bytearray`, a
plain `data class`, an ordinary class with writable attributes, `object`, `dynamic` — is unstable.

### 4.2 why the guarantee lives on the read side

a mutation of non-observable data is never a *trigger*: an immutable value cannot change, an
observable notifies when it does, a mutable value changes without telling anyone. so the sound
rule is that **a composition may only depend on immutable or observable values** — a parameter, a
global or a captured name read during composition must be one or the other. once that holds it does
not matter where a write happens (another file, a `.py` module, `dynamic`, a callback): either the
value cannot change, or the change notifies. the write-side rule (`silent-mutation`) stays because
it points at the exact line; the read-side rule (`unobservable-dependency`) is what makes the
property general.

### 4.2a the runtime invariant that holds regardless

> a composable is skipped only when every argument is provably stable and structurally equal;
> any other argument disables skipping for that scope.

so a mutable value can only render stale if it is mutated *and no state changes afterwards*; the
runtime is "slow but never stale" for unstable arguments, and the lints below turn the slow cases
into errors or warnings.

### 4.3 compile-time rules (fork; `TyCompat::BasedPython`; each with an mdtest; implemented on `ui-support`, real output in `docs/lints.md`)

| rule | level | fires when |
|---|---|---|
| `unobservable-dependency` | error | in composition-time code (the body, `once`/`local` content blocks, `derived`/`remember` lambdas) a load of a parameter, global or captured name whose type is neither deeply immutable nor an observable. a read-only `list[out T]` view counts as unobservable: it restricts this reader, not other writers. locals created by the composition itself are its own values and exempt. message: `` `items: list[str]` is read while `Names` composes, but nothing observes a change to it; hold it in state (`state_list`), pass an immutable value (`tuple[str, ...]`, a `frozen data class`), or read it only in a handler ``, with `` `Names` composes here `` annotated on the header; a global or captured name reads `` `TODOS` (`list[Todo]`) is read while `Names` composes, … make it immutable, or read it only in a handler `` |
| `mutable-state-value` | error | the value given to `state()` / `State.value =` / `state_list` elements / `StateDict` values / `remember` / `derived` is not stable: `` `list[int]` cannot be held in state: a change to it cannot be observed; use `state_list`, a `tuple`, or a `frozen data class` `` |
| `silent-mutation` | error | inside a composable body (content blocks folded in) or a handler block written in one: a mutating call (`append`, `extend`, `insert`, `pop`, `remove`, `clear`, `sort`, `reverse`, `update`, `setdefault`, `popitem`, `add`, `discard`, `__iadd__`, …) on a builtin mutable container, a subscript store on one, or an attribute store on a non-frozen, non-observable instance — unless the receiver is a fresh literal bound in the same body and never passed on |
| `state-write-in-composition` | error | `.value =`, `+=`, or a `StateList` / `StateDict` mutator in a composable body outside a callback scope (a non-`once` block, a lambda, a nested `def`, an effect block) |
| `conditional-slot` | warning | `state` / `remember` / `derived` / effects called under `if` / `for` / `while` / `try` or in a non-`once` block. the runtime keys slots by call site, so nothing is transplanted; the slot is created and disposed as the condition changes |
| `content-block-control-flow` | error | `return` / `break` / `continue` inside a content block — verified: a `return` two blocks deep propagates one level and is silently discarded |
| `escaping-loop-variable` (extended) | error | a handler block inside a `once` block inside a loop capturing the loop variable (the runtime trap above); the existing rule now sees through `once` blocks |
| `unstable-parameter` | warning | a composable parameter whose type is unstable: the scope never skips (a mutable parameter that only a handler touches is not a dependency, so this stays a warning about skipping). also shown as the `unstable` inlay hint |
| `unkeyed-children` | warning | *not implemented* — a composable called inside a loop whose body is not a keyed group. the runtime keys children by call site and ordinal, so an unkeyed loop is correct; it only re-creates scopes on reorder |
| `composable-outside-composition` | error | a composable or builder called from a function that is neither a composable, a content block, nor the root of `run_app` / `compose_test` |

what the checker rejects **today**, with no fork (all verified): rebinding a `let`; writing a field of
a `frozen data class`; `items.append(1)` and `items[0] = 2` on a `list[out int]` parameter (the
view stops this composable from writing, but it is still not observable, so reading it during
composition is `unobservable-dependency`; a composable that must accept a list takes a `tuple` or a
`StateList`); `return` in a
handler block; assigning an enclosing `let` from a handler block; a handler block directly inside a
loop capturing the loop variable; a missing `context` argument.

### 4.4 runtime guards (always on, cheap)

- writes to `State.value` / `StateList` / `StateDict` while the composer is composing raise
  `CompositionError` before mutating anything (one int compare per write)
- a value entering state is checked against a per-type cache of forbidden classes (`list`, `dict`,
  `set`, `bytearray`, non-frozen dataclasses) — the defence for `.py` callers and `dynamic` values
  the lints cannot see
- slot kind/order mismatch on recomposition raises with the composable's path
- writes from a thread other than the ui thread are posted, never applied in place — including a
  write made while the ui thread is composing, which is posted rather than refused (the worker
  cannot know the phase; the ui thread refuses only its own writes during composition)
- a `Derived` that reads itself raises
- a user exception during composition discards that scope's output, keeps the previously committed
  subtree, reports through `on_error`, and never touches the retained tree

### 4.5 honest holes

| hole | mitigation |
|---|---|
| mutation through a callee in another file (`helper(items)` appends) | closed by `unobservable-dependency`: a composition cannot read a mutable parameter or global in the first place, so the helper's write has nothing to make stale. an interprocedural `mutates` fact (F8) would only improve the message |
| `.py` callers and `dynamic` | state entry is guarded at runtime; a `dynamic`-typed read in composition is exempt from the read-side rule (gradual typing is exempt everywhere) |
| a `derived` / `remember` imported under another name, or a nested `def` passed to one | not recognised as composition-time code by the read-side rule (detection is by name); the composable body around it is still checked |
| a module attribute read during composition (`math.tau`) | exempt: a module is treated as a namespace, so a mutable module-level value is only checked when read through its own name (a global) |
| a frozen data class holding a mutable field typed `object` | rejected statically by `mutable-state-value`; the runtime guard checks declared field types one level down |
| a fresh lambda passed every composition | correct but never skipped; documented; `remember` a handler on hot paths |

## 5. dependency inference and inlay hints (fork; implemented on `ui-support`)

- **per composable** `reads`: the set of state places the composition depends on — `.value` on a
  `State` / `Derived`, iteration / `len` / subscript on a `StateList` / `StateDict`, `.current` on an
  `Ambient`, `context` parameters — including reads inside `once` content blocks and excluding
  handler blocks, lambdas, nested defs and effect blocks. interprocedural: a callee contributes the
  reads rooted at its parameters and globals, mapped through the argument expressions; its own
  cells are dropped. computed as a salsa least fixed point over the call graph — a clone of the
  `raises` inference (`exceptions.rs`), with the same cheap negative path
- **per `derived`**: `depends on` over the lambda body
- **per parameter**: `stable` / `unstable`
- **per state write** (F11): `invalidates` — the inverse of `reads`, at every observable write that
  runs after composition (a handler block, an `on_x=lambda`, a nested def, an effect block): the
  composables and derived computations whose composition depends on the place written, or
  `invalidates nothing` when no reader exists. computed hint-only (never from the lint walk) in
  `state_invalidations.rs`: the owner's own-scope reads (plain callees followed, composable callees
  stopped at, so a child that reads its parameter is named instead of the parent that forwards it;
  a child that takes a content block is inline and names its parent too), nested composables
  capturing the slot, composable callees handed the place one hop at a time, `derived` bindings
  whose lambda reads it and their readers, `root` for a `run_app` / `compose_test` / `set_root`
  block, an alias followed one binding deep, and a same-file sweep for module-level and
  parameter-rooted places, which always end in `…` because a reader in another file cannot be seen

rendering, in the harness's `[..]` notation — the first three lines are the fork's actual output on
`examples/counter.by`, `todo.by` and `form.by` (snapshot tests `basedpython_ui_*_example` in
`crates/ty_ide`):

```
def Counter(step: int = 1)[ reads count]:
def TodoApp()[ reads todos, draft, remaining]:                        # next_id is only read in a handler
def SignupForm(on_submit: (Signup) -> None)[ reads name, email, submitted, name_error, email_error, can_submit]:
    let name_error = derived(lambda: validate_name(name.value))[ depends on name]

def TodoList([unstable ]items: list[str])[ reads items]:
def opaque(cell: State[int], thing: dynamic) -> int[ reads cell, …]:
```

the write side, from the same snapshots (`basedpython_invalidations` and the counter example):

```
            Button("+"):
                count.value += step[ invalidates Counter]
            count.value += step[ invalidates Child, Display, Counter, total]
        Button("reset", on_click=lambda: count.set(0))[ invalidates Child, Display, Counter, total]
            unread.set(1)[ invalidates nothing]
    Button("click", on_click=lambda: CLICKS.set(1))[ invalidates Themed, Other, …]
```

settings: `inlayHints.inferredReads`, `inlayHints.parameterStability`, `inlayHints.derivedDependencies`,
`inlayHints.inferredInvalidations` (all on by default); the pycharm plugin classifies them by the
prefixes `reads `, `unstable`, `depends on `, `invalidates `.

each name is a label part that navigates to its declaration. the static set is a superset
approximation used only for hints and lints; invalidation always uses the exact runtime read set,
so imprecision can never cause a missed re-render. opaque callees add `…` to the hint. the
`invalidates` set inherits the same property in the other direction: it may name a scope that the
runtime, with its exact read set, does not re-run, and the trace (§6.7) is the exact answer.

## 6. runtime architecture

### 6.1 the three trees

| tree | owner | representation | lifetime |
|---|---|---|---|
| fragment (flutter's widget) | python, per scope, per execution | flat `array('i')` records (8 ints per node) + interned strings + `array('d')` canvas commands + a handler list | one frame; discarded on error |
| element / scope | python `Scope` objects for composition state; rust arena for identity and reconciliation | python: slots, deps, child scope ids, anchor; rust: `SlotMap<NodeId, Element{kind, key, parent, children, render}>` | retained |
| render | rust | `Vec<RenderNode>`: constraints cache, size, offset, intrinsic cache, text layout handle, modifier id, paint list id, dirty bits | retained |

### 6.2 composition

- a **scope** is one execution of a data-only composable. calling a composable looks up the child
  scope at the parent's current cursor (or in the enclosing `key` group); if it exists and every
  argument is stable and equal, a `SCOPE_REF` record is emitted and nothing runs (skip); otherwise
  the body runs with `current_scope` set, its cursor reset and its deps cleared
- a composable that takes a content block is **inline**: it re-runs with its parent, so no block is
  ever retained. this is what keeps `once` honest
- **slots**: `state(x)` reads `scope.slots[cursor]` if present (kind checked) else creates;
  `cursor += 1`. slot identity is positional; `key(k):` opens a keyed group so loop iterations match
  across reorders. scope identity is the calling site (`f_code`, `f_lasti` of the caller frame,
  ~240 ns, taken only for composable calls) plus the key and an ordinal; a transpiler pass in the
  fork injects a literal `_site=N` later, which also makes compiled composables possible
- **recomposition** of a dirty scope runs it in isolation with its retained data arguments; its
  output replaces its old range; child scopes are reconciled exactly like the first run. dirty
  scopes run parents first; a scope its parent re-ran is skipped

### 6.3 state and notification

- `State.value` get: if a scope is composing, record the cell (and its version); set: phase check,
  thread check, `if new == old: return` (structural equality — values are immutable, so this is
  safe), bump the version, invalidate every subscribed scope (insert into the dirty heap)
- **batching**: invalidation only inserts; nothing runs synchronously. a handler that writes ten
  cells causes one recomposition
- **glitch-freedom**: `Derived` is pull-based with version stamps — it re-validates by comparing
  each dependency's version, recomputes at most once per version tuple and notifies its readers only
  if the value changed. composition reads deriveds after all of the frame's writes are applied, so a
  scope never observes `name_error` stale next to `can_submit` fresh
- **ordering** within a frame: posted writes → disposals of removed scopes (cancel tasks, run
  `on_dispose` cleanups in reverse registration order, unsubscribe) → recompositions parents-first →
  one commit → layout → `side_effect`s and new effect tasks → paint. effects observe a committed tree
- **re-entrancy**: writes during composition raise; a handler sees its own writes; recomposition
  never runs inside a handler; a write-in-effect ping-pong is capped at eight frames with a
  diagnostic

### 6.4 commit, layout, paint, input (rust)

- `commit(ints, floats, strs, ranges)` validates the record stream before touching the tree
  (malformed input is a `ValueError`, never a panic; `catch_unwind` at the boundary marks the core
  poisoned). per dirty range it reconciles children: keyed children through a `(key, kind)` map,
  unkeyed by position; props diffed (text bytes → reshape only on change; modifier ids interned);
  `needs_layout` / `needs_paint` propagate to the nearest boundary. measured boundary cost: 9 ns per
  node for a 10k-node fragment through the buffer protocol (a per-node call would be 47 ns and
  would lose atomic commits)
- **layout**: single pass, constraints down, sizes up, per-node `(constraints → size)` cache,
  intrinsic caches, relayout boundaries; flex (`weight`) in two passes within a node; a custom
  `Layout` calls the python measure function once per layout of that node (documented slow path)
- **text**: cosmic-text / parley shaping with a cache keyed by (text, style, max width); glyphs in an
  atlas. cold shaping is the first-frame cost; cached is free
- **paint**: each repaint boundary owns a display list; the frame rasterises the union of damage
  rects with `tiny-skia` into a `softbuffer` surface (always available, also the headless path);
  `wgpu` / `vello` replaces the rasteriser behind the same display list later. `Canvas` blocks run
  at paint time, batched, into the float buffer
- **input**: `winit` events → rust hit test on the render tree → `(handler index, event)` → python
  handler → state writes → frame. focus and text editing state live in rust; `TextField` sees
  committed strings

### 6.5 threading

the ui thread is the main thread (winit requires it on macos): events, composition, commit, layout,
paint. one asyncio loop on a task thread runs `launched_effect` bodies; cross-thread state writes
are posted through `EventLoopProxy`, never applied in place. the rust core is built per interpreter
(not abi3: abi3 modules do not load on free-threaded 3.14t) and declares `Py_MOD_GIL_NOT_USED`.
paint moves to a raster thread once display lists are double-buffered.

### 6.6 python versus rust, and why

python holds user closures and user values: composition, slots, keys, the state graph, deriveds,
ambients, effect scheduling, handler tables, error policy. rust holds everything that is O(nodes)
per frame and touches no user object: arenas, reconciliation, layout, text, paint, raster, hit
test, window, timers. the boundary carries ints, floats and strings only, once per frame in each
direction.

### 6.7 the trace: why a scope ran

the runtime keeps a bounded ring of records — one per scope run (its origin and every cause: a
state write with old and new value and the writer's site and thread, a derived recompute, an
argument that differed, an inline restart, a recovery), one per state write, one per frame, one
per error and per refused write — as exact-builtin tuples whose layout is
[the trace protocol](trace-protocol.md) (`TRACE_FORMAT`). causes are threaded from the public
mutator through `changed(cause)` → `on_dep_changed(cell, cause)` → `invalidate(scope, cause)` and
recorded *before* the dirty de-duplication, so one run carries every write that caused it. every
append is announced with `sys.audit("basedpython_ui.trace", record)`; `bpd` hooks that natively
and forwards records while a client watches through a bounded queue that never blocks the ui
thread, and reads the ring off `live_runtimes` at a stop through storage alone. in process,
`Runtime.explain()` renders the ring and `TestComposition.why(fn)` returns the records of one
composable, matched by identity, with locations mapped to `.by` lines through `_by_sourcemap` when
its digests still match. tracing is on by default; `trace=False` costs one attribute test per run
and per write. the user-facing account is the guide page `docs/guide/why-did-this-rerender.md`; the
debugger and editor surfaces are `docs/guide/debugger.md`

## 7. performance

measured on this machine (apple silicon, cpython 3.14.7) during design:

| cost | measured |
|---|---|
| the transpiled block dsl, tree building only | 160 ns per node (10k nodes in 1.6 ms) |
| composition plus flat emission, realistic 10k-node tree | 227 ns per node (246 on 3.14t) |
| `commit` boundary, 50k ints + 10k strings via the buffer protocol | 9 ns per node |
| one pyo3 call per node | 47 ns per node (rejected) |
| caller-frame site key | 237 ns per composable call |
| tracked `State.value` read | 86 ns (72 untracked) |
| `_soundness_iter` wrapper | +10 ns per element (release builds use `--soundness none`) |
| a `functools.partial` for an extension member inside a block | ~150 ns (framework builders are functions, so this applies only to user extensions) |
| a `= Modifier()` default | ~300 ns per omitted call (the api uses `let` sentinels: a name load) |

measured on the implemented runtime (`bench/compose_bench.by`, null core, cpython 3.14.7, soundness
off), after binding hot names eagerly and caching interned styles by identity:

| shape | first composition | recompose everything | skip an unchanged child scope |
|---|---|---|---|
| 10k nodes in 100 scopes | 1.4 µs / node | 0.72 µs / node | ~1.5 µs |
| one scope per leaf node | 3.2 µs / node | 2.7 µs / node | ~1.5 µs |

the trace (§6.7) is on by default; measured in the same bench with tracing on and off, each mode in
its own process, two runs each, before the machine was loaded by other builds:

| shape | tracing | first composition | recompose everything | skip an unchanged child scope |
|---|---|---|---|---|
| 10k nodes in 100 scopes | off | 1.32–1.37 µs / node | 0.71–0.75 µs / node | ~1.8 µs |
| 10k nodes in 100 scopes | on | 1.21–1.27 µs / node | 0.73–0.78 µs / node | ~1.8–2.1 µs |
| one scope per leaf node | off | 4.0–4.1 µs / node | 3.2–3.4 µs / node | ~12–16 µs (dominated by the per-frame scope loop) |
| one scope per leaf node | on | 5.3–5.4 µs / node | 3.9–4.1 µs / node | ~12–14 µs |

so a scope pays roughly 0.7 µs per run for its record (two `perf_counter_ns` calls, the 15-slot
tuple, the ring append, one `sys.audit`), which is noise for scopes of tens of nodes and +20–30 % for
the pathological one-scope-per-leaf shape; an app with thousands of one-node scopes can pass
`run_app(trace=False)`. the cProfile top twelve with tracing off contains no trace function.

the default runtime soundness wrappers (`_soundness_iter`, `_soundness_parametric`, … — see
`by build --soundness`) cost about 30 % on composition (0.77 µs against 0.60 µs per node on a full
recomposition); release builds pass `--soundness none`. the fork's `by check` on the whole project,
all lints included, takes 0.15 s.

what the profile taught: basedpython's lazy imports lower `from pkg import name` to a proxy object,
and a proxy costs a python-level call on every use — a builder called through two re-export layers
paid two extra calls per node, and the record constants were reaching the emitter as proxies. the
framework binds its hot names to the real objects at import time; a self-rebinding proxy (one that
replaces the importing module's global on first resolution) is on the fork list because user code
pays the same cost on every builder call. a scope costs about 1 µs over a plain node (call-site
lookup, argument comparison, subscription bookkeeping), so leaf composables that emit one node are
the wrong granularity — the same advice as compose's.

targets ("high performance", concretely):

| scenario, 10k retained nodes | compose | commit | layout | paint | total | 120 hz |
|---|---|---|---|---|---|---|
| one state change, one scope of ~50 nodes | 15 µs | 10 µs | 30 µs | 0.3–1 ms | ≤ 1.5 ms | yes |
| scroll a list by one line | 0 | 0 | 50 µs | 1–2 ms | ≤ 2.5 ms | yes |
| full rebuild (theme switch) | 2.3 ms | 1 ms | 1.5 ms | 3–4 ms software | ≈ 8–9 ms | needs the gpu backend |

free threading: the native module is built per interpreter and declares itself gil-free; the whole
test suite, real core included, passes on 3.14t, and writes to any observable from a worker thread
are posted to the ui thread rather than applied in place (a stress test appends from eight threads
and loses nothing).

plus: input-to-paint latency ≤ one frame; idle cpu 0 %; ≤ 200 B of python per node (only scopes and
handlers are python objects); ≤ 256 B of rust per node. `by compile` is applied to the framework's
`runtime` module (undecorated, base-less classes with scalar fields) for roughly 2× on the emission
share; user composables stay interpreted; the rust core is outside the compilation unit, which is
exactly the batched boundary above.

## 8. the compiler fork (`basedpython`, branch `ui-support`)

recognition and guarantees (in order):

| # | change | effort |
|---|---|---|
| F1 | *done* — recognise the framework: `KnownModule::BasedpythonUi` (also accepted first-party, so the framework can be developed in place), `KnownClass::{State, StateList, StateDict, Derived, Ambient}`, `KnownFunction::composable` → `FunctionDecorators::COMPOSABLE`, `FunctionFrameworkRole::Composable`, `dedicated/basedpython_ui.rs` helpers | 2 d |
| F2 | `Type::is_deeply_immutable(db, env)` (salsa, `cycle_initial = true`), modelled on the frozen-field / final / private polarity rules variance inference already uses | 3 d |
| F3 | *done* — checker fixes the api worked around: a call carrying a trailing block resolves `context` arguments and literal conversions like any other call; a `context` parameter may precede a trailing `once` callable parameter; a block's `it` on a generic free function is typed from the solved specialization; `escaping-loop-variable` sees through `once` blocks; a nested block whose callback passes nothing declares no `it`; a `let` inside a block never gets `nonlocal`; a conversion target reached through a re-export is imported from its declaring module | 4 d |
| F4 | *done* — the lints of §4.3 (`crates/ty_python_semantic/src/types/composition.rs` + `immutability.rs`) with a literate mdtest per rule; `Type::is_deeply_immutable` is exposed as `ty_extensions._internal.is_deeply_immutable` | 6 d |
| F5 | *done* — `state_reads` inference (`crates/ty_python_semantic/src/types/state_reads.rs`) + the three inlay hints (`InlayHintKind::{Reads, Stability, DerivedDeps}`, settings, server options, docs, snapshot tests, pycharm `ByHintKind`) | 5 d |
| F6 | transpiler pass injecting `_site=N` into composable calls (exact identity without frames; enables compiled composables) | 2 d |
| F7 | optional sugar: `composable def` modifier keyword lowered to `@composable` | 1 d |
| F8 | later: interprocedural `mutates` fact for `silent-mutation` across files (message quality only: the read side is closed by F10) | 5 d |
| F9 | lazy-import proxies rebind the importing module's global on first resolution (removes a python call per use of every imported name; measured as the largest single cost in the emitter) | 1 d |
| F11 | *done* — the `Invalidates` inlay hint (`inferredInvalidations`): at every observable write that runs after composition, ` invalidates Counter, total` or ` invalidates nothing`, the inverse of the read set computed by hint-only salsa queries in `state_invalidations.rs` (owner, inline children and their parent, nested composables, composable callees handed the place, derived transitivity, aliases one binding deep, same-file sweep with `…`); `open_window` dead code removed | 2 d |
| F12 | *done* — the transpiler's line map for hoisted trailing-lambda blocks (`by_transforms/src/source_map.rs`): a replacement remembers the runs it was assembled from (copied source, generated text with an anchor) and each output line is charged to the first copied text on it, else to the anchor of the generated text; before, every line of a hoisted block mapped to the block-owning statement's line, so a handler write, a traceback and a breakpoint inside a handler all landed on the wrong `.by` line | 1 d |
| F10 | *done* — `unobservable-dependency`, the read-side rule of §4.2 (`composition.rs`, `Composition::read_timing`, `dependency_kind` over the place tables; `derived` / `remember` lambdas registered as standalone expressions by `ty_python_core/src/builder.rs`), the corrected `unstable-parameter` message, ten mdtest sections including the cross-file case | 2 d |

nothing framework-specific is encoded in the fork beyond "these names are observables / this
decorator marks a scope / this predicate is stability", so the framework can evolve without compiler
releases.

## 9. milestones

**M1 — vertical slice (framework repo only).** *status: done on 2026-09-02 — see `native/README.md`; `tests/test_harness.by` clicks, types and toggles through the real core, and `examples/counter.by` opens in a window.* `runtime.by` (cells, scopes, slots, keys, dirty heap,
phase guards, error containment, disposal, skipping with a runtime stability table), `widgets.by`
emitting fragments, the rust core (`native/`: winit + softbuffer + tiny-skia + cosmic-text, arenas,
reconcile, flex/box/padding layout, hit test, `commit` / `poll_events` / `frame`, headless surface),
`run_app`, `compose_test`. runs end to end: `counter.by` and `todo.by` in a window and under
`compose_test`, with `test_counter.by` asserting recomposition counts; a benchmark script for
compose / commit / frame.

**M2 — fork, recognition and the checker fixes (F1–F3).** *done.* the workarounds in the examples came out.

**M3 — fork, safety (F4).** *done.* the seven lints; `docs/lints.md` shows them on a file of mistakes.

**M4 — fork, inference (F5–F6).** *F5 done:* `reads` / `depends on` / `unstable` in the editor. *F6 open:* `_site`
injection; the runtime prefers injected sites when present.

**M5 — framework breadth.** effects with the task thread, `Ambient` / `provide`, `TextField` /
`Checkbox` editing and focus, `Canvas`, custom `Layout`, `LazyColumn`, animation clock, damage
rects; every example runs.

**M6 — performance.** `by compile` of `runtime`, raster thread, `wgpu` / `vello`, 3.14t validation,
the benchmark ledger in ci with the targets of §7.

## 10. language findings recorded along the way

each of these was reproduced against `by` on 2026-09-02 and drives a line in F3. items 3, 4, 6, 7,
8, 9 and 10 are fixed on the fork's `ui-support` worktree (each with an mdtest; the examples no
longer carry the workarounds); 1, 2 and 5 are language facts the api is designed around:

1. a class constructor cannot take a trailing block (the block is prepended positionally)
2. a decorator typed `[**P](fn: (**P) -> None) -> (**P) -> None` turns the function into a callable
   type and loses `once`; the identity-typed `[F](fn: F) -> F` keeps the function literal
3. a call carrying a trailing block does not resolve `context` arguments or literal conversions
4. `parameter after a context parameter must also be context` is a parse error, so a composable
   cannot declare both a `context` parameter and a content block
5. `return` in a block nested in a block propagates one level and is discarded; the checker is
   silent when a fallthrough `return` exists
6. a handler block inside a `once` block inside a loop closes over the shared loop variable at
   runtime (prints the last item every time) while type-checking cleanly
7. a nested block whose callback passes no argument still declares `it=None`, shadowing the outer
   block's `it` at runtime; the checker resolves the name outward
8. a `let` inside a block whose name is also bound in the enclosing function is lowered with both
   `nonlocal` and a `Final` annotation, which python rejects
9. a literal conversion whose target class is imported through a package re-export fails at
   transpile time ("declared in a module this file does not import") although `by check` passes
10. a block's `it` on a generic free function stays the unsolved `T`; on a bound method it is
    specialised
