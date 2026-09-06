# the compiler's rules

the mutation-safety rules of [state and mutation safety](state.md) are checks the compiler fork
runs under `by check` (`TyCompat::BasedPython`, each with an mdtest; under the `ty-compatible`
preset they are off). this page is the table of them, and then a file of deliberate mistakes with
the verbatim output, so what each rule catches — and what it deliberately leaves alone — can be
read off real diagnostics

## the rules

| rule | level | fires when |
| --- | --- | --- |
| `unobservable-dependency` | error | in composition-time code (the body, `once` / `local` content blocks, `derived` / `remember` lambdas) a load of a parameter, global or captured name whose type is neither deeply immutable nor an observable. a read-only `list[out T]` view counts as unobservable: it restricts this reader, not other writers. locals created by the composition itself are its own values and exempt. message: `` `items: list[str]` is read while `Names` composes, but nothing observes a change to it; hold it in state (`state_list`), pass an immutable value (`tuple[str, ...]`, a `frozen data class`), or read it only in a handler ``, with `` `Names` composes here `` annotated on the header; a global or captured name reads `` `TODOS` (`list[Todo]`) is read while `Names` composes, … make it immutable, or read it only in a handler `` |
| `mutable-state-value` | error | the value given to `state()` / `State.value =` / `state_list` elements / `StateDict` values / `remember` / `derived` is not stable: `` `list[int]` cannot be held in state: a change to it cannot be observed; use `state_list`, a `tuple`, or a `frozen data class` `` |
| `silent-mutation` | error | inside a composable body (content blocks folded in) or a handler block written in one: a mutating call (`append`, `extend`, `insert`, `pop`, `remove`, `clear`, `sort`, `reverse`, `update`, `setdefault`, `popitem`, `add`, `discard`, `__iadd__`, …) on a builtin mutable container, a subscript store on one, or an attribute store on a non-frozen, non-observable instance — unless the receiver is a fresh literal bound in the same body and never passed on |
| `state-write-in-composition` | error | `.value =`, `+=`, or a `StateList` / `StateDict` mutator in a composable body outside a callback scope (a non-`once` block, a lambda, a nested `def`, an effect block) |
| `conditional-slot` | warning | `state` / `remember` / `derived` / effects called under `if` / `for` / `while` / `try` or in a non-`once` block. the runtime keys slots by call site, so nothing is transplanted; the slot is created and disposed as the condition changes |
| `content-block-control-flow` | error | `return` / `break` / `continue` inside a content block — verified: a `return` two blocks deep propagates one level and is silently discarded |
| `escaping-loop-variable` (extended) | error | a handler block inside a `once` block inside a loop capturing the loop variable (the runtime trap on the [state page](state.md#the-cells)); the existing rule now sees through `once` blocks |
| `unstable-parameter` | warning | a composable parameter whose type is unstable: the scope never skips (a mutable parameter that only a handler touches is not a dependency, so this stays a warning about skipping). also shown as the `unstable` [inlay hint](inlay-hints.md) |
| `unkeyed-children` | warning | *not implemented* — a composable called inside a loop whose body is not a keyed group. the runtime keys children by call site and ordinal, so an unkeyed loop is correct; it only re-creates scopes on reorder |
| `composable-outside-composition` | error | a composable or builder called from a function that is neither a composable, a content block, nor the root of `run_app` / `compose_test` |

what the checker rejects **today**, with no fork (all verified): rebinding a `let`; writing a
field of a `frozen data class`; `items.append(1)` and `items[0] = 2` on a `list[out int]`
parameter (the view stops this composable from writing, but it is still not observable, so reading
it during composition is `unobservable-dependency`; a composable that must accept a list takes a
`tuple` or a `StateList`); `return` in a handler block; assigning an enclosing `let` from a handler
block; a handler block directly inside a loop capturing the loop variable; a missing `context`
argument

## a file of mistakes

`mistakes.by` is a composable for each mistake, and `helpers.by` is a mutation that lives in
another file. below them is the verbatim output of `by check mistakes.by` from the fork's
`ui-support` branch on 2026-09-03, with only the empty gutter lines removed

```by
from basedpython_ui import composable, state, state_list, Column, Text, Button, TextField
from helpers import add_item


frozen data class Todo:
    id: int
    title: str


data class Draft:                      # mutable: not frozen
    text: str


@composable
def MutableInState():
    let items = state([1, 2, 3])       # a list cannot be observed
    Text(str(len(items.value)))


@composable
def SilentMutation(items: list[str], draft: Draft):
    Column:
        Text(str(len(items)))
        Button("add"):
            items.append("x")          # the composition cannot see this
        Button("edit"):
            draft.text = "changed"     # nor this


@composable
def WriteWhileComposing():
    let count = state(0)
    count.value = 1                    # composition is not the place to write
    Text(str(count.value))


@composable
def ConditionalSlot(flag: bool):
    if flag:
        let extra = state(0)
        Text(str(extra.value))


@composable
def CrossFile(items: list[str]):
    Text(str(len(items)))              # the composition depends on a list nothing observes
    Button("add"):
        add_item(items, "x")           # the helper in the other file mutates it


def helper():
    Text("outside")                    # no composition is running here


def main():
    helper()
```

```by
# helpers.by
def add_item(items: list[str], item: str):
    items.append(item)                 # the mutation lives in another file
```

## what `by check` says

```text
error[mutable-state-value]: `list[int]` cannot be held in state: a change to it cannot be observed; use `state_list`, a `tuple`, or a `frozen data class`
  --> mistakes.by:16:23
16 |     let items = state([1, 2, 3])       # a list cannot be observed
   |                       ^^^^^^^^^

warning[unstable-parameter]: `items: list[str]` is unstable, so `SilentMutation` is never skipped; prefer `tuple[str, ...]`, `state_list`, or a `frozen data class`
  --> mistakes.by:21:20
21 | def SilentMutation(items: list[str], draft: Draft):
   |                    ^^^^^^^^^^^^^^^^

warning[unstable-parameter]: `draft: Draft` is unstable, so `SilentMutation` is never skipped; prefer a `frozen data class` or an observable
  --> mistakes.by:21:38
21 | def SilentMutation(items: list[str], draft: Draft):
   |                                      ^^^^^^^^^^^^

error[unobservable-dependency]: `items: list[str]` is read while `SilentMutation` composes, but nothing observes a change to it; hold it in state (`state_list`), pass an immutable value (`tuple[str, ...]`, a `frozen data class`), or read it only in a handler
  --> mistakes.by:23:22
21 | def SilentMutation(items: list[str], draft: Draft):
   |     ---------------------------------------------- `SilentMutation` composes here
22 |     Column:
23 |         Text(str(len(items)))
   |                      ^^^^^

error[silent-mutation]: `items.append(...)` mutates `list[str]` in place, which `SilentMutation`'s composition cannot observe; mutate a `StateList` or rebuild an immutable value
  --> mistakes.by:25:13
21 | def SilentMutation(items: list[str], draft: Draft):
   |     ---------------------------------------------- `SilentMutation` composes here
22 |     Column:
23 |         Text(str(len(items)))
24 |         Button("add"):
25 |             items.append("x")          # the composition cannot see this
   |             ^^^^^^^^^^^^^^^^^

error[silent-mutation]: `draft.text = ...` mutates `Draft` in place, which `SilentMutation`'s composition cannot observe; mutate a `StateList` or rebuild an immutable value
  --> mistakes.by:27:13
27 |             draft.text = "changed"     # nor this
   |             ^^^^^^^^^^
  ::: mistakes.by:21:5
21 | def SilentMutation(items: list[str], draft: Draft):
   |     ---------------------------------------------- `SilentMutation` composes here

error[state-write-in-composition]: `count` is written while `WriteWhileComposing` is composing; move the write into an event handler or an effect
  --> mistakes.by:33:5
33 |     count.value = 1                    # composition is not the place to write
   |     ^^^^^^^^^^^

warning[conditional-slot]: `state()` under a condition: it will be created and disposed as the condition changes
  --> mistakes.by:40:21
40 |         let extra = state(0)
   |                     ^^^^^^^^

warning[unstable-parameter]: `items: list[str]` is unstable, so `CrossFile` is never skipped; prefer `tuple[str, ...]`, `state_list`, or a `frozen data class`
  --> mistakes.by:45:15
45 | def CrossFile(items: list[str]):
   |               ^^^^^^^^^^^^^^^^

error[unobservable-dependency]: `items: list[str]` is read while `CrossFile` composes, but nothing observes a change to it; hold it in state (`state_list`), pass an immutable value (`tuple[str, ...]`, a `frozen data class`), or read it only in a handler
  --> mistakes.by:46:18
45 | def CrossFile(items: list[str]):
   |     --------------------------- `CrossFile` composes here
46 |     Text(str(len(items)))              # the composition depends on a list nothing observes
   |                  ^^^^^

error[composable-outside-composition]: `Text` is a builder and can only be called while composing
  --> mistakes.by:52:5
52 |     Text("outside")                    # no composition is running here
   |     ^^^^

Found 11 diagnostics
```

## what is and is not caught, on purpose

- `unobservable-dependency` is the read side of the guarantee: a composition may only depend on
    immutable or observable values, so a `list` parameter read in the body is reported at the read,
    wherever the write that would make it stale lives. `CrossFile` above depends on `items`; the
    `append` is in `helpers.by`, which is not reported and needs no rule of its own. `draft: Draft`
    in `SilentMutation` is *not* reported by it: the body never reads `draft`, only a handler writes
    it, and that write is `silent-mutation`'s
- `mutable-state-value` fires at the value's entry into state, so the mutable object never gets
    the chance to be mutated behind the composition's back
- `silent-mutation` covers the composable's body, its content blocks, and every handler, lambda or
    effect written inside it; a fresh local container that never leaves the body is exempt
- `unstable-parameter` is a warning: the scope is correct, just never skipped; the fix is a
    `tuple`, `state_list` or a `frozen data class`. a `list[out T]` view stops this composable from
    writing but is still not observable, so a read of it is `unobservable-dependency`
- `conditional-slot` is a warning: the runtime keys slots by call site, so nothing is
    transplanted, but the slot is created and disposed as the condition changes
- what remains invisible to the checker is on the state page under
    [honest holes](state.md#honest-holes): `.py` callers and `dynamic` values (exempt from every
    rule; the runtime guards cover those), a `derived` / `remember` imported under another name
    (not recognised as composition-time code), and a module attribute read during composition
    (`math.tau`: a module is treated as a namespace)
