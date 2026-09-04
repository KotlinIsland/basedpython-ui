# what the compiler says

the mutation-safety rules of docs/design.md §4.3 are implemented on the fork's `ui-support` branch.
this file is `mistakes.by` (a composable for each mistake) and `helpers.by` (a mutation that lives
in another file), followed by the verbatim output of `by check mistakes.by` from that branch on
2026-09-03, with only the empty gutter lines removed.

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

what is and is not caught, on purpose:

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
- what remains invisible to the checker is documented in §4.5: `.py` callers and `dynamic` values
  (exempt from every rule; the runtime guards cover those), a `derived` / `remember` imported under
  another name (not recognised as composition-time code), and a module attribute read during
  composition (`math.tau`: a module is treated as a namespace)
