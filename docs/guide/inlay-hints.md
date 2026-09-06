# inlay hints

the compiler fork infers four things about a composition and `by server` renders each as an inlay
hint: what a composable reads, what a `derived` depends on, which parameters are unstable, and —
after a state write — which composables the write re-runs. each is a checker fact, computed the way
the `raises` inference is (a least fixed point over the call graph), and each can be turned off on
its own through the `ty.inlayHints.<name>` setting an editor passes to the server

## the notation

the docs write a hint the way the editor renders it, between angle brackets, so a `by` fence can
show what was inferred without the hint being source:

```by
def Counter(step: int = 1)⟨ reads count⟩:
```

nothing between the brackets is in the file. a hint that goes after something starts with a space
(` reads count`); a hint that goes before something ends with one (`unstable `)

## the four hints

| setting | shows | where it sits |
| --- | --- | --- |
| `inferredReads` | ` reads count, items` — the observables the composition depends on | after the composable's header |
| `derivedDependencies` | ` depends on name` — what a `derived(...)` computation depends on | after the `derived(...)` call |
| `parameterStability` | `unstable ` — a parameter the runtime cannot compare, so the scope never skips | before the parameter |
| `inferredInvalidations` | ` invalidates Counter, Total` — the composables a state write re-runs; ` invalidates nothing` when nothing reads the cell | after a statement that writes a cell |

all four default to on. each name in a hint is a label part that navigates to its declaration. the
pycharm plugin classifies a hint by its fixed prefix — `reads `, `depends on `, `unstable`,
`invalidates ` — and offers every kind as never, always or push-to-hint

the first three lines below are the fork's actual output on `examples/counter.by`, `todo.by` and
`form.by` (the snapshot tests `basedpython_ui_*_example` in `crates/ty_ide` of the fork):

```by
def Counter(step: int = 1)⟨ reads count⟩:
def TodoApp()⟨ reads todos, draft, remaining⟩:                        # next_id is only read in a handler
def SignupForm(on_submit: (Signup) -> None)⟨ reads name, email, submitted, name_error, email_error, can_submit⟩:
    let name_error = derived(lambda: validate_name(name.value))⟨ depends on name⟩

def TodoList(⟨unstable ⟩items: list[str])⟨ reads items⟩:
def opaque(cell: State[int], thing: dynamic) -> int⟨ reads cell, …⟩:
```

## what counts as a read

the `reads` set of a composable is every state place its composition depends on: `.value` on a
`State` / `Derived`, iteration / `len` / subscript on a `StateList` / `StateDict`, `.current` on an
`Ambient`, and `context` parameters — including reads inside `once` content blocks, and excluding
handler blocks, lambdas, nested defs and effect blocks, which run later. `next_id` above is read
only inside the `add` handler, so `TodoApp` does not read it

it is interprocedural: a callee contributes the reads rooted at its parameters and globals, mapped
through the argument expressions, and its own cells are dropped. a callee that cannot be followed —
one reached through a `dynamic` value — adds `…` to the hint rather than letting the set look
complete

## the write side: `inferredInvalidations`

the fourth hint is the inverse of the read set. after every observable write that runs *after*
composition — a handler block, an `on_x=lambda`, a nested def, an effect block — it names what the
write may re-run:

```by
Button("+"):
    count.value += step⟨ invalidates Counter⟩
Button("reset", on_click=lambda: count.set(0)⟨ invalidates Child, Display, Counter, total⟩)
unread.set(1)⟨ invalidates nothing⟩
CLICKS.set(1)⟨ invalidates Themed, Other, …⟩
```

the hint sits at the end of its own site — a simple statement's end, a lambda's end — so two
writes on one line get two hints. a write while composing gets no hint: that is a lint

what it names is the composables and derived computations whose composition depends on the place
written, resolved the way the runtime subscribes: the composable that owns the slot when its own
scope reads it (content blocks and plain helpers included); a child handed the slot as an argument,
directly, through a helper or across modules, named instead of the parent that merely forwards it;
a child called with a content block together with its parent, because such a child is inline and
re-runs whenever the parent does; a `derived` whose lambda reads the place and, through it, whatever
reads the derived; the `root` of a `run_app`, `compose_test` or `set_root` block. a slot declared
inside a `Column:` block belongs to the composable that owns the block, and an alias is followed
binding by binding (`let cell = model.count; cell.set(1)` names what reads `model.count`)

`nothing` is an affirmative statement, reserved for a resolved slot that no composition reads. `…`
means the set may be larger than what is shown: a module-level slot or a parameter (a reader in
another file cannot be seen), a written name that is not a slot (a loop or comprehension target, a
value bound in a handler, a subscript), an unpacked argument that may fill a parameter, a `dynamic`
callee, and a slot of a composable whose last parameter is callable, since a caller the walk cannot
see may hand it a content block and be subscribed — a `private def` composable is exempt from that
last one

the set is static and a superset, like the read set: it may name a scope the runtime, with its exact
read set, does not re-run, and it never misses one within a file. `reads` and `invalidates` differ
on a forwarding parent on purpose — `reads` lifts a callee's parameter reads into the caller so the
caller's hint is complete, while `invalidates` names the child because the runtime subscribes the
child's own scope and skips an unchanged parent. the exact answer is the trace:
[why did this rerender](why-did-this-rerender.md)

## static and a superset; the trace is exact

the static set is an approximation used only for hints and lints. invalidation at runtime always
uses the exact set of readers a cell had when it was written, so an imprecise hint can never mean
a missed re-render — the hint can name a composable that this particular write did not reach,
never miss one that it did

so the two answers to "what does this write re-run" are different things.
` invalidates Counter, Total` is what a write *may* re-run, known before the program runs; the
runtime's trace says what a write *did* re-run, with the value before and after, the frame it
happened in, and how many readers were notified — the write record with `readers == 0` is the
runtime's ` invalidates nothing`. that is [why did this rerender](why-did-this-rerender.md), and
at a stop the debugger puts the two side
by side: the hint on the write, the record on the composable it reached

## limitations

### the read set is approximate

reads are recovered statically by following calls. a callee reached through a `dynamic` value
cannot be followed, and the hint says so with `…` rather than claiming the set is complete. the
same `…` appears on an invalidation hint whose write site reaches an opaque callee

### a hint is not a lint

`unstable ` and `unstable-parameter` are the same fact — one shown, one reported — and the other
three hints report nothing. a hint that names a composable you did not expect is a reason to look
at the trace, not a diagnostic
