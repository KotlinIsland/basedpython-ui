# the native core protocol (`basedpython_ui._native`)

the rust core owns the retained element tree, layout, text, painting, hit testing and the window.
the basedpython side owns composition and state. they meet at one batched `commit` per frame in one
direction and one `poll_events` per frame in the other. nothing crosses the boundary per node.

## module and classes

module `basedpython_ui._native` (pyo3 0.29, `extension-module`, built per interpreter — not abi3,
so it loads on 3.14t too; declares `Py_MOD_GIL_NOT_USED`). the built `.so` is placed in
`src/basedpython_ui/` (gitignored) so `by build` carries it into `out/basedpython_ui/`.

```
class Core:
    def __new__(cls, width: float, height: float, scale: float = 1.0) -> Core
    def commit(self, ints, floats, strs, ranges, new_modifiers, new_styles) -> list[int]
    def layout(self) -> None
    def paint(self) -> None
    def resize(self, width: float, height: float, scale: float) -> None
    def hit_test(self, x: float, y: float) -> int          # handler index or -1
    def find_text(self, text: str) -> tuple[int, float, float, float, float] | None   # node id, x, y, w, h
    def node_rect(self, node_id: int) -> tuple[float, float, float, float] | None
    def dump(self) -> str                                    # indented tree with kinds, text, rects
    def node_count(self) -> int
    def pixels(self) -> bytes                                # rgba8, width*height*4, headless paint target
    def last_frame_ms(self) -> tuple[float, float, float]    # commit, layout, paint

class Window:
    def __new__(cls, title: str, width: float, height: float, icon: bytes | None = None) -> Window
        # `icon` is an 8-bit rgba png. it becomes the window's icon where the platform has
        # one, and on macos the *application's* icon (what the dock shows), which is set
        # through AppKit because it does not belong to a window at all
    def run(self, on_frame, on_events) -> None
        # runs the winit event loop on the calling (main) thread.
        # on_events(events: list[tuple]) is called with pending input events before a frame;
        # on_frame(core: Core) is called when a redraw is due; it must call core.commit/layout/paint
        # as needed and return True to request another frame soon (animation) or False to wait.
    def request_frame(self) -> None                          # thread-safe wake-up (EventLoopProxy)
    def close(self) -> None
```

## the fragment: `ints`

`ints` is a buffer of `i32` (python `array('i')`, exposed through the buffer protocol; the core must
not copy it per element beyond one pass). records are fixed width: **8 ints per record**:

```
[kind, flags, text_idx, modifier_id, handler_idx, a, b, c]
```

| kind | name | payload |
|---|---|---|
| 0 | END | closes the innermost open container / scope |
| 1 | SCOPE | opens a scope group; `a` = scope id. a scope group is transparent for layout (its children are laid out as children of the enclosing container) |
| 2 | SCOPE_REF | `a` = scope id: keep the retained group for that scope exactly as it is (the scope was skipped) |
| 14 | POPUP | container; `a` = x, `c` = y in logical pixels. see below |
| 3 | COLUMN | container; `a` = arrangement (0 start, 1 center, 2 end, 3 space-between, 4 space-evenly), `c` = cross-axis alignment (0 start, 1 center, 2 end; a child's own `align` modifier wins); children follow until END |
| 4 | ROW | container; as COLUMN |
| 5 | BOX | container; `a` = alignment (0 start, 1 center, 2 end) |
| 6 | TEXT | `text_idx`; `a` = style id |
| 7 | BUTTON | `text_idx` = label; `handler_idx` = on_click; `a` = enabled (0/1); `b` = style id |
| 8 | TEXTFIELD | `text_idx` = value; `b` = placeholder text idx (or -1); `handler_idx` = on_change |
| 9 | CHECKBOX | `a` = checked (0/1); `handler_idx` = on_change |
| 10 | SPACER | |
| 11 | CANVAS | `a` = first float index, `b` = float count of its draw commands (see floats) |
| 12 | LAYOUT | custom layout; `handler_idx` = measure callback index; children follow until END |

`flags`: bit 0 = the record carries a key; bit 1 = the key is a string (then `b` is a `strs` index)
else the key is the int in `b`. keys matter only for children of containers: keyed children are
reconciled by `(key, kind)`, unkeyed ones by position among the unkeyed.

`modifier_id`: index into the core's interned modifier table (see `new_modifiers`); 0 is the empty
modifier.

`handler_idx`: an index into the frame's python handler list, which the python side keeps; the
core only stores and returns it (`hit_test`, events). -1 = none.

`text_idx`: index into `strs` for this commit; -1 = none. the core copies the string only when it
differs from the retained node's text (compare bytes first).

## `floats`

a buffer of `f64` (python `array('d')`). canvas draw commands are runs of `[op, n args…]`:

| op | args |
|---|---|
| 1 rect | x, y, w, h, argb (as f64 of the u32) |
| 2 circle | cx, cy, r, argb |
| 3 line | x1, y1, x2, y2, argb, stroke |
| 4 text | x, y, text_idx, style id |

## `ranges`

`list[tuple[int, int, int]]` of `(scope_id, first_record, end_record)`: each range is the new
content of that scope's group; the core replaces the group's children with the records
`[first, end)` and disposes whatever they no longer contain. the root is scope 0 and its group is
the root element. a `SCOPE` record for scope `s` inside a range creates or reuses the group for `s`;
a `SCOPE_REF` reuses it untouched. the return value lists the scope ids whose groups were
disposed (removed subtrees), so python can dispose their `Scope` objects.

validation happens before any mutation: unbalanced END, unknown kinds, out-of-range indices are a
`ValueError` and leave the tree untouched. a rust panic anywhere is caught at the boundary,
converted to `RuntimeError`, and poisons the core (every later call raises).

## `new_modifiers` and `new_styles`

`new_modifiers: list[tuple[int, list[float]]]` — `(id, ops)` for ids the core has not seen; ops are
runs of `[op, …]`:

| op | args |
|---|---|
| 1 padding | left, top, right, bottom |
| 2 width | value |
| 3 height | value |
| 4 fill max width | |
| 5 fill max height | |
| 6 background | argb |
| 7 weight | value |
| 8 align | 0/1/2 |
| 9 clickable | handler index (frame-local) |

`new_styles: list[tuple[int, float, float, int]]` — `(style id, size, argb, bold)`.

## layout

single pass, constraints down, sizes up. `Column` / `Row` measure unweighted children loosely,
split the remaining main-axis space by `weight`, then place by arrangement. `Box` stacks children
with alignment. modifiers apply in order (padding shrinks the child constraints and grows the size;
width/height make the constraint tight; fill uses the max constraint; background paints behind).
text is measured with cosmic-text (system font fallback, fixed 14px default) and cached by (text,
style, max width). a per-node `(constraints → size)` cache skips clean subtrees. `LAYOUT` nodes call
back into python: the measure callback receives `(child_count, min_w, max_w, min_h, max_h)` and must
return `(w, h)` after calling `core.measure_child(index, min_w, max_w, min_h, max_h) -> (w, h)` and
`core.place_child(index, x, y)` — these two methods exist on `Core` and are only valid inside the
callback.

## paint

each container is a repaint boundary in M1 (simplest correct thing). paint walks the tree into a
display list and rasterises with `tiny-skia` into an rgba buffer; the window blits it through
`softbuffer`. glyphs come from cosmic-text's `SwashCache`. damage rects are M6.

## input

`Window.run` collects winit events into `(kind, x, y, handler_idx, text)` tuples: kinds 1 pointer
down, 2 pointer up, 3 pointer move, 4 key text, 5 resize, 6 close. for pointer events the core has
already hit-tested and filled `handler_idx` (buttons and clickables get 1 (down) / 2 (up) pairs;
python dispatches `on_click` on up if the down landed on the same handler). text fields: the core
owns focus and the edit buffer; it emits kind 4 events with the committed text and the field's
handler index.

## amendments (2026-09-02, after the runtime design)

1. **canvas after layout.** a draw block needs the laid-out size, so the CANVAS record carries only
   `handler_idx` (`a`/`b` unused). after `layout()`, `core.canvas_requests()` returns
   `[(node_id, handler_idx, width, height), …]` for every canvas whose size changed or whose record
   was (re)committed; python runs the draw block and calls
   `core.set_canvas_commands(node_id, floats, strs)`; the core retains the commands per node until
   replaced and `paint()` uses them.
2. **layout callback.** `Core.layout(measure=None)`: `measure(handler_idx, child_count, min_w, max_w,
   min_h, max_h) -> (w, h)` is invoked for each LAYOUT node; inside it `core.measure_child(index,
   min_w, max_w, min_h, max_h) -> (w, h)` and `core.place_child(index, x, y)` are valid. a python
   exception in the callback propagates out of `layout()`; the node is zero-sized for that pass.
3. **key / `b` / `c` conflicts.** the key is in `b` as stated above, with two exceptions that
   free `b` where a kind already uses it: TEXTFIELD carries its key in `c` (`b` stays the
   placeholder index), and BUTTON carries its style id in `c` (`b` is its key). so:
   `BUTTON: a = enabled, b = key, c = style`; `TEXTFIELD: b = placeholder, c = key`; every other
   kind: `b = key`. a string key (flag bit 1) is a `strs` index in that field. flag bits other
   than 0 and 1 are a `ValueError`; scope records carry no key (flags must be 0).
4. **`commit`'s `floats` is ignored** (amendment 1 moved canvas commands to
   `set_canvas_commands`). pass an empty `array('d')`. `ints` / `floats` may be any contiguous
   buffer of the right element type (`array('i')` / `array('d')`, a memoryview); an empty buffer
   is fine. a wrong type is a `TypeError`.

5. **two test-harness queries** the python side needs: `all_text() -> list[str]` (every visible
   text in tree order: TEXT, BUTTON labels, TEXTFIELD values, or the placeholder when the value is
   empty) and `field_handler(placeholder: str) -> int` (the `on_change` handler index of the
   first TEXTFIELD with that placeholder, or -1).
## decisions taken by the native implementation (2026-09-02)

these pin down what the protocol left open. they are the behaviour of `native/`; the python side
can rely on them.

### ranges and scopes

- ranges are **record indices** (`first`, `end` count 8-int records, not ints; the error for an
  int offset says so), are processed in the given order (python emits parents first) and must be
  **disjoint** record intervals. a scope id may be *either* a range's scope *or* a `SCOPE` record in one commit,
  never both (two definitions of its content). `SCOPE_REF s` in the parent's range plus a range
  for `s` is the normal "parent re-ran, `s` was skipped but is itself dirty" case.
- every scope id occurs at most once as a `SCOPE` / `SCOPE_REF` record per commit; scope 0 is the
  root and never appears as a record; a `SCOPE_REF` needs a retained group. a `SCOPE` /
  `SCOPE_REF` of an existing group inside the range for `s` must currently be a descendant of
  `s`'s group (groups only ever move within their parent scope — a cycle is impossible).
- a range for `s` replaces the children of `s`'s group; groups that nothing references any more
  are disposed (recursively) at the end of the commit and their ids returned, in disposal order.
- keyed children match by `(key, kind)` anywhere among the container's old children; duplicate
  keys in one container are a `ValueError`. unkeyed children match the k-th unkeyed old child
  when kinds agree, else the old one is disposed and a new one created. scope groups are not
  counted as positional children.
- all of this is checked before any mutation. one exception is documented in `commit.rs`: the
  modifier / style tables are extended before the records are validated (harmless: interning is
  idempotent), so a rejected commit can leave new ids interned.

### interning

- modifier ids are positive u32 (0 is the empty modifier); style ids are u32 with 0 = the default
  (14 px, `0xFF202020`, regular). re-sending an id with identical content is ignored; different
  content is a `ValueError`. colour floats must be integral in `0..=0xFFFFFFFF`; paddings, widths
  and heights must be finite and `>= 0`; `weight > 0`; `align` in 0/1/2; the clickable handler
  index is a non-negative int.
- the core keeps one `Arc<Modifier>` per node (no lookup at layout); `weight` / `align` are the
  last such op in the chain and are read by the parent.

### layout

- the root group behaves like a `Box` filling the window: children measured loosely and placed at
  the origin. all sizes are logical pixels (f32).
- modifiers apply outermost first in list order. `padding` shrinks the child constraints and
  grows the result; `width` / `height` make the axis tight (clamped into the incoming
  constraints); `fill max width/height` sets the min to the max when the max is finite;
  `background` and `clickable` cover the rect of the layer they sit at (so `padding(16).clickable`
  excludes the padding, `clickable.padding(16)` includes it).
- `Column` / `Row`: unweighted children are measured with main `(0, remaining)` and cross
  `(0, max)`; weighted children then split the remaining main space in proportion, with tight
  main constraints (the last weighted child takes the rounding remainder). with an unbounded main
  axis weighted children are measured loosely. the container's main size is the max constraint
  when anything is weighted, else `clamp(sum, min, max)`; the cross size is `clamp(max child, min,
  max)`. arrangement: 0 start, 1 center, 2 end, 3 space-between (gap = free / (n-1)), 4
  space-evenly (gap = free / (n+1)). a child's `align` modifier positions it on the cross axis.
- `Box`: the single alignment value applies to both axes (0 top-left, 1 centre, 2 bottom-right);
  a child's `align` modifier overrides it. children are measured loosely.
- `Text` wraps at `max_w`; `Button` is its label plus 12 px / 6 px padding; `TextField` is
  `max(text width, 120) + 16` by `line height + 12`; `Checkbox` is 18×18; `Spacer` and `Canvas`
  take the minimum constraints, so they get their size from modifiers or `weight`.
- text: sans-serif family Helvetica (macOS) / Segoe UI (windows) / DejaVu Sans (else) with
  cosmic-text's platform fallbacks; line height `ceil(1.25 × size)`; measured sizes are rounded up
  to whole pixels. with no fonts at all the core uses a monospace estimate (0.6 × size per
  character) instead of panicking, and draws nothing.
- the `(constraints → size)` cache: a commit marks changed nodes and their ancestors dirty; a
  clean subtree under unchanged constraints is skipped; `layout()` on a clean tree does nothing.
- custom `LAYOUT` nodes are re-measured on every `layout()` (their policy lives in python), and so
  are their ancestors; siblings still hit the cache. children the callback did not
  `measure_child` are laid out with zero constraints; children it did not `place_child` sit at
  (0, 0). nested `LAYOUT` nodes work. inside the callback the mutating `Core` methods (`commit`,
  `layout`, `paint`, `resize`, `canvas_requests`, `set_canvas_commands`, `pointer`, `key_*`) raise
  `RuntimeError`; the queries (`hit_test`, `find_text`, `node_rect`, `dump`, `node_count`,
  `pixels`, …) work. `measure_child` constraints must be finite-or-inf, `>= 0` and `min <= max`
  (`ValueError`), the index in range (`IndexError`); the callback must return a pair of finite
  non-negative numbers (`TypeError` / `ValueError`). a nested callback's exception is raised
  from the enclosing `measure_child` *and* from `layout()`, whichever python frames sit between.

### canvas

- `canvas_requests()` clears the pending list as it returns it; a canvas is pending after its
  record is (re)committed and whenever layout changes its size. `set_canvas_commands` validates
  every op (`ValueError`, nothing retained on error) and replaces the node's commands; the text
  op's `text_idx` indexes the `strs` passed to that call, its style id must be interned. commands
  are in the canvas's own coordinates and are not clipped to it (TODO).

### input

- `hit_test` walks later siblings and children first (top-most wins). an enabled `Button` and a
  `Checkbox` report their `handler_idx`; a disabled `Button` and a `TextField` consume the hit and
  report -1; a clickable modifier reports the innermost clickable layer containing the point. the
  core does not know what a handler does: python registers a zero-argument closure per click-type
  handler (for a checkbox, e.g. `lambda: on_change(not checked)`).
- `pointer(kind, x, y)` performs the hit test and, on kind 1, focuses the text field hit (caret
  at the end) or blurs. `key_text(text)` inserts at the caret and returns
  `(4, 0, 0, field_handler, new_value)`; with no focus it returns `(4, 0, 0, -1, text)` so python
  can implement shortcuts. `key_named(name)` handles Backspace, Delete, Left, Right, Home, End,
  Escape (blur), Tab (next field in tree order), Enter (ignored with focus); without focus Enter,
  Tab, Escape, Backspace, Delete produce `"\n"`, `"\t"`, `"\x1b"`, `"\x08"`, `"\x7f"`. caret moves
  return None. on commit the field's python value wins: a different value replaces the buffer
  (caret clamped); disposing the focused field blurs.
- the `Window` maps winit events onto exactly these methods. resize is `(5, w, h, -1, "")` in
  logical pixels (the core is already resized); close is `(6, 0, 0, -1, "")`, delivered to
  `on_events` and then the loop exits; consecutive moves are coalesced into the latest one, and a
  frame is requested when the hovered handler changes; Cmd/Ctrl-modified keys are not inserted;
  IME composition (preedit) is not supported in M1 — plain `KeyEvent.text` is used.

### window

- `Window(title, width, height)` records the configuration; `run(on_frame, on_events)` creates
  the event loop on the calling thread (which must be the main thread; one loop per process, so
  one `run` per process), creates the window, and creates the `Core` with the real logical size
  and scale factor. the GIL is released while the loop waits; callbacks re-attach. a python
  exception in `on_frame` / `on_events` ends the loop and is raised from `run`. `request_frame`
  and `close` are thread-safe and are no-ops before `run` starts. the window blits the core's
  last painted buffer after each `on_frame` (if `on_frame` did not paint, the previous frame is
  shown again).

### extra `Core` methods and formats

```
def all_text(self) -> list[str]                              # amendment 5
def field_handler(self, placeholder: str) -> int             # amendment 5
def size(self) -> tuple[float, float, float]                 # width, height, scale
def pixel_size(self) -> tuple[int, int]                      # round(width*scale), round(height*scale)
def canvas_requests(self) -> list[tuple[int, int, float, float]]
def set_canvas_commands(self, node_id: int, floats, strs) -> None
def pointer(self, kind: int, x: float, y: float) -> tuple    # the protocol event tuple
def key_text(self, text: str) -> tuple | None
def key_named(self, name: str) -> tuple | None
def focused_handler(self) -> int
def layout(self, measure=None) -> None                       # amendment 2
```

- `pixels()` is `pixel_width * pixel_height * 4` bytes of premultiplied RGBA8 painted over an
  opaque white background (so straight == premultiplied); zeros before the first `paint()`.
- `node_count()` counts elements, not scope groups. node ids are opaque ints (arena key +
  generation); a stale id yields `None` from `node_rect`.
- `find_text` matches TEXT, BUTTON labels and TEXTFIELD values exactly, in tree order, then
  TEXTFIELD placeholders.
- `dump()` is one line per node, two spaces per depth: `Root [WxH]`, `Scope <id>`, or
  `<Kind> "text" [x,y WxH] mod=<id> handler=<idx> …` with kind-specific extras
  (`arrangement=`, `alignment=`, `disabled`, `checked=`, `placeholder=`, `focused`, `key=`).
- `last_frame_ms()` reports the durations of the last `commit`, `layout`, `paint` calls.
- errors: `ValueError` for malformed input (tree untouched), `TypeError` for wrong argument
  types, `IndexError` for a bad child index, `RuntimeError` for state errors (callback required,
  re-entrancy, a poisoned core). a rust panic anywhere becomes `RuntimeError` and poisons the
  core (every later call raises); a `Window` panic ends `run` with `RuntimeError`.
- the module declares `Py_MOD_GIL_NOT_USED` (`#[pymodule(gil_used = false)]`); `Core` is
  `Send + Sync` (its state is behind a mutex; the font database is process-wide behind another),
  `Window` too, but its loop is bound to the thread that calls `run`.

## amendments (2026-09-04, for basedgit-ui)

added for the first application built on the framework: a scroll container, decorated islands,
hover, keyboard chords and the modifier keys of pointer events.

### SCROLL (kind 13)

a container; `a` = axis and must be 0 (vertical), `c` = cross-axis alignment (0/1/2);
children follow until END. children are laid
out like a `Column` with an unbounded height, the node takes the height its constraints give it
(`fill_max_height` / `weight` make it the viewport), and its painting, its children's painting
and hit testing are clipped to its content rect. the core owns the scroll offset (clamped to
`[0, content - viewport]` on every layout); `Core.scroll(x, y, dy)` moves the innermost
container under the point that can still move that way and returns whether anything moved
(the `Window` maps the mouse wheel onto it: one line = 40 logical pixels). a thin thumb is
painted at the right edge while the content overflows, and the content is then laid out one
gutter (8 px) narrower so the thumb never covers it — narrowing can only make the content
taller, so the second measuring pass is always the last. `dump()` shows `scroll=` and
`content=`.

### modifier ops 10–15

| op | args | effect |
|---|---|---|
| 10 rounded | tl, tr, br, bl | corner radii, clockwise from the top left, for every background, border, shadow and hover layer after it in the chain. radii that do not fit a side are scaled down together (the css rule), so adjacent rounded nodes still share an edge exactly |
| 11 border | width, argb | a stroke inside the layer's rect (no layout effect) |
| 12 hover | argb | painted over the chain's clickable layer while `hovered_handler()` is its handler |
| 13 shadow | elevation, argb | a soft shadow under the layer's rect, painted before the layers after it |
| 14 reveal | | when a node's modifier *gains* this op (a new node, or a modifier change), layout scrolls the nearest enclosing SCROLL so the node is inside its viewport, once |
| 15 clip | | the node's own painting and its children are clipped to the node's rect |
| 16 secondary | handler index | the right button over this layer |
| 17 hoverable | handler index | told when the pointer enters and leaves this layer |
| 18 scrollbar | argb | the colour of this scroll container's thumb (the default is a translucent black) |
| 19 draggable | handler index | told when the pointer is pressed here and dragged; the core holds the pointer until it comes up |

### style flags

`new_styles` entries are `(id, size, argb, flags)`; `flags` is a bit set: 1 bold, 2 monospace
family, 4 no-wrap (one line, clipped to the node's content rect when wider). other bits are a
`ValueError`. `Style::DEFAULT` is unchanged (flags 0).

### POPUP (kind 14)

content that floats over the window: a menu, a tooltip. it is **outside the flow** — the
container it is written in neither measures nor places it — it is laid out against the window
(loose constraints), placed at `(a, c)` clamped so it stays inside, painted after everything
else, hit before everything else, and no ancestor's `clip` applies to it. what lands on a part
of it that answers nothing carries on down the tree, so a tooltip does not eat the click under
it; a menu that wants the clicks around it keeps its own full-window popup behind it.
`dump()` shows `at=x,y`.

### hover

`Core.pointer(kind, x, y)` records the **node** under the pointer for every kind (also 3,
move); `Core.hovered_handler()` reads that node's click handler as of the last layout, and an
enabled `Button` under the pointer is painted darker. tracking the node rather than the handler
index is what keeps a hover correct across a recomposition, which hands every handler a new
index.

a node carrying op 17 also gets **enter / leave events**: when the hovered node changes, the
core queues a kind-11 event for the node being left (`text` = "") and one for the node being
entered (`text` = "1"), each carrying that node's current hoverable handler and the pointer
position. `Core.take_events()` drains the queue (the `Window` does this after every pointer event);
`Core.hover_target()` is the hoverable handler under the pointer, or -1.

### dragging

a press over an op-19 layer **captures the pointer**: the core remembers the node, and every
move until the release belongs to it however far the pointer has travelled — which is what a
splitter needs, since the pointer leaves its few pixels immediately. the captured press, the
moves and the release are queued as kind-12 events (`text` = "start" / "move" / "end") and the
ordinary pointer events for them report handler -1, so nothing else reads them as a click.

### events

- kind **7 KEY_CHORD**: `(7, 0, 0, -1, chord)`. the `Window` emits it for every key pressed
  while cmd, ctrl or alt is held, and for up / down / pageup / pagedown / f1–f12 always
  (those keys mean nothing to a text field). the chord is `ctrl+alt+shift+cmd+<key>` with
  only the held modifiers present and the key in lower case ("cmd+x", "shift+up",
  "pagedown"). plain keys keep the kind-4 contract (typed text with the field's handler, or
  handler -1 with the text / control character when nothing is focused); the python side
  turns the unfocused kind-4 texts into chord names (`chord_of_text`).
- pointer events (kinds 1, 2, 3) carry the held modifiers in `text` ("", "shift",
  "shift+cmd", …) so a click handler can read them (`input_modifiers()`).
- kinds **9 / 10** are the right button down and up. they hit-test `on_secondary` layers only,
  falling through buttons, checkboxes and text fields, so a row can offer a menu without every
  widget on it forwarding one. python dispatches on the up when it lands on the same handler
  as the down, exactly as it does for the left button.
- kind **11** is a hover enter or leave, and kind **12** a drag step (see above).
- kind **8 THEME**: `(8, 0, 0, -1, "light" | "dark")`, the appearance the window follows,
  sent when the window opens (when the platform reports one) and on every change; the
  python side keeps it in `basedpython_ui.app.system_theme`, a `State[str]`; the resize
  event (kind 5) likewise reaches `basedpython_ui.app.window_size`, a `State[(w, h)]`.

### extra `Core` methods

```
def hovered_handler(self) -> int
def hover_target(self) -> int
def take_events(self) -> list[tuple]                         # kind 11 and 12 events since the last call
def scroll(self, x: float, y: float, dy: float) -> bool
def scroll_offset(self, node_id: int) -> float | None          # SCROLL nodes only
def first_scroll(self) -> tuple[int, float, float, float] | None   # (node id, offset, content, viewport), tests
```

### painting

buttons, text fields and checkboxes have rounded corners; plain rectangles are clipped
arithmetically, paths through a tiny-skia mask built once per distinct clip rect per frame,
glyphs per pixel.
