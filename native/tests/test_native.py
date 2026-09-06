"""Boundary tests for basedpython_ui._native, driven with hand-built fragments.

Run from the repository root:
    PYTHONPATH=src python3.14 -m pytest native/tests -q -s
"""

import time
from array import array

import pytest

from basedpython_ui._native import Core

# record kinds
END_K, SCOPE, SCOPE_REF, COLUMN, ROW, BOX, TEXT, BUTTON, TEXTFIELD, CHECKBOX, SPACER, CANVAS, LAYOUT = range(13)

END = (END_K, 0, -1, 0, -1, 0, 0, 0)


def rec(kind, flags=0, text=-1, modifier=0, handler=-1, a=0, b=0, c=0):
    return (kind, flags, text, modifier, handler, a, b, c)


def ints(*records):
    out = array("i")
    for r in records:
        out.extend(r)
    return out


def commit(core, records, strs=(), ranges=None, mods=(), styles=()):
    buf = ints(*records)
    if ranges is None:
        ranges = [(0, 0, len(records))]
    return core.commit(buf, array("d"), list(strs), ranges, list(mods), list(styles))


PADDING16 = (1, [1.0, 16.0, 16.0, 16.0, 16.0])
SIZE50 = (2, [2.0, 50.0, 3.0, 50.0])
SIZE100x50 = (3, [2.0, 100.0, 3.0, 50.0])


def basic(core):
    strs = ["hello", "world", "click me"]
    records = [rec(COLUMN, modifier=1), rec(TEXT, text=0), rec(TEXT, text=1), rec(BUTTON, text=2, handler=7, a=1), END]
    return commit(core, records, strs, mods=[PADDING16])


def pixel(px, width, x, y):
    i = (int(y) * width + int(x)) * 4
    return tuple(px[i : i + 4])


def test_commit_layout_paint_and_queries():
    core = Core(400.0, 300.0)
    assert basic(core) == []
    core.layout()
    core.paint()
    assert core.node_count() == 4

    found = core.find_text("hello")
    assert found is not None
    node_id, x, y, w, h = found
    assert (x, y) == (16.0, 16.0)
    assert w > 0 and h > 0
    assert core.node_rect(node_id) == (x, y, w, h)
    assert core.node_rect(123456789) is None

    bx, by, bw, bh = core.find_text("click me")[1:]
    assert by >= y + h  # below the two texts
    assert core.hit_test(bx + bw / 2, by + bh / 2) == 7
    assert core.hit_test(bx - 1, by + 1) == -1
    assert core.hit_test(399, 299) == -1

    px = core.pixels()
    assert len(px) == 400 * 300 * 4
    assert pixel(px, 400, bx + 2, by + bh / 2) != (255, 255, 255, 255)  # button fill
    assert pixel(px, 400, 398, 298) == (255, 255, 255, 255)  # background

    dump = core.dump()
    for needle in ("hello", "world", "click me", "Column", "Button", "handler=7"):
        assert needle in dump, dump
    c, l, p = core.last_frame_ms()
    assert c >= 0 and l >= 0 and p >= 0
    assert core.size() == (400.0, 300.0, 1.0)
    assert core.pixel_size() == (400, 300)


def test_second_commit_with_scope_ref_keeps_subtree():
    core = Core(400.0, 300.0)
    strs = ["hello", "world", "changed"]
    records = [rec(COLUMN), rec(SCOPE, a=1), rec(TEXT, text=0), END, rec(TEXT, text=1), END]
    assert commit(core, records, strs) == []
    core.layout()
    hello_id = core.find_text("hello")[0]
    assert core.node_count() == 3

    records = [rec(COLUMN), rec(SCOPE_REF, a=1), rec(TEXT, text=2), END]
    assert commit(core, records, strs) == []
    core.layout()
    assert core.find_text("hello")[0] == hello_id
    assert core.find_text("world") is None
    assert core.find_text("changed") is not None
    assert "Scope 1" in core.dump()

    # a range for scope 1 alone replaces only its content; the parent is untouched
    records = [rec(TEXT, text=1)]
    assert commit(core, records, strs, ranges=[(1, 0, 1)]) == []
    core.layout()
    assert core.find_text("hello") is None
    assert core.find_text("world")[0] == hello_id  # positional reuse of the text node
    assert core.find_text("changed") is not None

    # dropping the scope disposes it and reports the id
    records = [rec(COLUMN), rec(TEXT, text=2), END]
    assert commit(core, records, strs) == [1]
    core.layout()
    assert core.node_count() == 2
    assert "Scope 1" not in core.dump()


def test_malformed_input_raises_value_error_and_leaves_tree_unchanged():
    core = Core(400.0, 300.0)
    basic(core)
    core.layout()
    before = core.dump()
    strs = ["a"]
    bad = [
        ([rec(COLUMN), rec(TEXT, text=0)], strs, None),  # unclosed container
        ([rec(TEXT, text=0), END], strs, None),  # stray END
        ([rec(99)], strs, None),  # unknown kind
        ([rec(TEXT, text=5)], strs, None),  # text index out of range
        ([rec(TEXT, text=0, modifier=42)], strs, None),  # unknown modifier
        ([rec(TEXT, text=0)], strs, [(0, 0, 2)]),  # range out of bounds
        ([rec(TEXT, text=0)], strs, [(7, 0, 1)]),  # range for an unknown scope
        ([rec(SCOPE_REF, a=9)], strs, None),  # unknown scope ref
        ([rec(COLUMN, a=9), END], strs, None),  # bad arrangement
        ([rec(TEXT, flags=1, text=0, b=1), rec(TEXT, flags=1, text=0, b=1)], strs, None),  # duplicate keys
        ([rec(BUTTON, text=0, a=1, c=77)], strs, None),  # unknown button style (in c)
        ([rec(LAYOUT), END], strs, None),  # layout without a handler
    ]
    for records, s, ranges in bad:
        with pytest.raises(ValueError):
            commit(core, records, s, ranges=ranges)
        assert core.dump() == before
    with pytest.raises(ValueError):
        core.commit(array("i", [1, 2, 3]), array("d"), [], [(0, 0, 0)], [], [])
    with pytest.raises(ValueError):
        commit(core, [], mods=[(1, [1.0, 1.0])])  # truncated padding op
    with pytest.raises(ValueError):
        commit(core, [], mods=[(1, [1.0, 1.0, 1.0, 1.0, 1.0])])  # same id, different ops
    with pytest.raises((TypeError, BufferError)):
        core.commit(b"\x00" * 8, array("d"), [], [], [], [])  # wrong buffer element type
    assert core.dump() == before


def test_keyed_children_reorder_keeps_nodes():
    core = Core(400.0, 300.0)
    strs = ["a", "b", "c"]
    records = [rec(COLUMN), rec(TEXT, flags=1, text=0, b=10), rec(TEXT, flags=1, text=1, b=11), rec(TEXT, flags=1, text=2, b=12), END]
    commit(core, records, strs)
    core.layout()
    ids = [core.find_text(t)[0] for t in strs]
    records = [rec(COLUMN), rec(TEXT, flags=1, text=2, b=12), rec(TEXT, flags=1, text=0, b=10), rec(TEXT, flags=1, text=1, b=11), END]
    commit(core, records, strs)
    core.layout()
    assert [core.find_text(t)[0] for t in strs] == ids
    assert core.find_text("c")[2] < core.find_text("a")[2]  # c is now on top
    # string keys live in the strs table; a text field's key is in c (b is its placeholder)
    records = [rec(COLUMN), rec(TEXT, flags=3, text=0, b=1), rec(TEXTFIELD, flags=1, text=0, b=2, c=5), END]
    commit(core, records, strs)
    dump = core.dump()
    assert "key=\"b\"" in dump and "key=5" in dump and 'placeholder="c"' in dump


def test_layout_callback_path():
    core = Core(400.0, 300.0)
    records = [rec(COLUMN), rec(LAYOUT, handler=3), rec(SPACER, modifier=2), rec(SPACER, modifier=2), END, rec(TEXT, text=0), END]
    commit(core, records, ["below"], mods=[SIZE50])
    seen = []

    def measure(handler_idx, n, min_w, max_w, min_h, max_h):
        seen.append((handler_idx, n, min_w, max_w, min_h, max_h))
        for i in range(n):
            w, h = core.measure_child(i, 0.0, max_w, 0.0, max_h)
            assert (w, h) == (50.0, 50.0)
            core.place_child(i, i * 60.0, i * 10.0)
        return (200.0, 100.0)

    core.layout(measure=measure)
    assert seen == [(3, 2, 0.0, 400.0, 0.0, 300.0)]
    dump = core.dump()
    assert "Layout [0,0 200x100]" in dump
    assert "Spacer [60,10 50x50]" in dump
    assert core.find_text("below")[2] == 100.0
    # custom layouts are re-measured every pass
    core.layout(measure=measure)
    assert len(seen) == 2

    with pytest.raises(RuntimeError):
        core.layout()  # callback required
    with pytest.raises(RuntimeError):
        core.measure_child(0, 0, 1, 0, 1)  # only valid inside the callback
    with pytest.raises(RuntimeError):
        core.place_child(0, 0, 0)

    def raises(*_):
        raise KeyError("boom")

    with pytest.raises(KeyError):
        core.layout(measure=raises)
    assert "Layout [0,0 0x0]" in core.dump()  # zero-sized for that pass, tree consistent
    assert core.find_text("below")[2] == 0.0

    def reenters(*_):
        with pytest.raises(RuntimeError):
            core.paint()
        with pytest.raises(RuntimeError):
            core.layout()
        with pytest.raises(IndexError):
            core.measure_child(5, 0, 1, 0, 1)
        with pytest.raises(ValueError):
            core.measure_child(0, 10, 1, 0, 1)
        assert core.node_count() == 5  # read-only queries are fine (column, layout, 2 spacers, text)
        return (10, 10)

    core.layout(measure=reenters)
    assert "Layout [0,0 10x10]" in core.dump()

    with pytest.raises(TypeError):
        core.layout(measure=lambda *a: "nope")
    with pytest.raises(TypeError):
        core.layout(measure=42)


def test_canvas_requests_and_commands():
    core = Core(200.0, 100.0)
    commit(core, [rec(CANVAS, modifier=3, handler=2)], mods=[SIZE100x50])
    core.layout()
    reqs = core.canvas_requests()
    assert len(reqs) == 1
    node_id, handler, w, h = reqs[0]
    assert (handler, w, h) == (2, 100.0, 50.0)
    assert core.canvas_requests() == []
    red = float(0xFFFF0000)
    core.set_canvas_commands(node_id, array("d", [1, 0, 0, 100, 50, red, 2, 20, 20, 5, float(0xFF0000FF), 4, 2, 2, 0, 0]), ["hi"])
    core.paint()
    px = core.pixels()
    assert pixel(px, 200, 50, 40) == (255, 0, 0, 255)
    assert pixel(px, 200, 150, 40) == (255, 255, 255, 255)
    # unchanged commit + layout: no new request; a size change requests again
    commit(core, [rec(CANVAS, modifier=3, handler=2)], mods=[])
    core.layout()
    assert len(core.canvas_requests()) == 1  # record was recommitted
    core.resize(300.0, 100.0, 1.0)
    core.layout()
    assert core.canvas_requests() == []  # size unchanged (fixed 100x50)
    with pytest.raises(ValueError):
        core.set_canvas_commands(node_id, array("d", [9.0]), [])
    with pytest.raises(ValueError):
        core.set_canvas_commands(node_id, array("d", [4, 0, 0, 3, 0]), ["only one"])
    with pytest.raises(ValueError):
        core.set_canvas_commands(424242, array("d", []), [])


def test_text_field_focus_and_editing():
    core = Core(400.0, 300.0)
    commit(core, [rec(COLUMN), rec(TEXTFIELD, text=0, handler=4, b=1), rec(BUTTON, text=2, handler=9, a=1), END], ["hi", "type here", "ok"])
    core.layout()
    _, x, y, w, h = core.find_text("hi")
    assert core.find_text("type here")[0] == core.find_text("hi")[0]  # placeholder matches too
    assert core.all_text() == ["hi", "ok"]
    assert core.field_handler("type here") == 4
    assert core.field_handler("nope") == -1
    assert core.key_text("z") == (4, 0.0, 0.0, -1, "z")  # nothing focused: shortcut-style event
    assert core.pointer(1, x + 3, y + 3) == (1, x + 3, y + 3, -1, "")
    assert core.focused_handler() == 4
    assert core.key_text("!") == (4, 0.0, 0.0, 4, "hi!")
    assert core.key_named("Backspace") == (4, 0.0, 0.0, 4, "hi")
    assert core.key_named("Left") is None
    assert core.key_text("X") == (4, 0.0, 0.0, 4, "hXi")
    # python confirms the value on the next commit; the buffer follows python
    commit(core, [rec(COLUMN), rec(TEXTFIELD, text=0, handler=4, b=1), rec(BUTTON, text=2, handler=9, a=1), END], ["hXi", "type here", "ok"])
    core.layout()
    core.paint()
    assert "focused" in core.dump()
    bx, by, bw, bh = core.find_text("ok")[1:]
    down = core.pointer(1, bx + 1, by + 1)
    up = core.pointer(2, bx + 1, by + 1)
    assert down[3] == 9 and up[3] == 9
    assert core.focused_handler() == -1
    with pytest.raises(ValueError):
        core.pointer(7, 0, 0)


def test_commit_10k_nodes_timing():
    t0 = time.perf_counter()
    core = Core(800.0, 600.0)
    create_ms = (time.perf_counter() - t0) * 1000  # includes the one-time font database load
    strs = [f"item {i}" for i in range(50)] + ["Row", "go"]
    records = [rec(COLUMN)]
    n = 1
    while n < 10_000:
        records.append(rec(ROW, flags=1, b=n))
        records.append(rec(TEXT, text=n % 50))
        records.append(rec(TEXT, text=(n * 7) % 50))
        records.append(rec(BUTTON, text=51, handler=n % 100, a=1))
        records.append(END)
        n += 4
    records.append(END)
    buf = ints(*records)
    ranges = [(0, 0, len(records))]
    t0 = time.perf_counter()
    core.commit(buf, array("d"), strs, ranges, [], [])
    first = (time.perf_counter() - t0) * 1000
    count = core.node_count()
    assert count >= 10_000
    core.layout()
    core.paint()
    _, layout_ms, paint_ms = core.last_frame_ms()
    best = float("inf")
    total = 0.0
    runs = 20
    for _ in range(runs):
        t0 = time.perf_counter()
        core.commit(buf, array("d"), strs, ranges, [], [])
        dt = (time.perf_counter() - t0) * 1000
        best = min(best, dt)
        total += dt
    core.layout()
    _, relayout_ms, _ = core.last_frame_ms()
    print(
        f"\n{count} nodes: core creation {create_ms:.1f} ms (font db), first commit {first:.3f} ms, "
        f"unchanged commit best {best:.3f} ms / mean {total / runs:.3f} ms (target <= 1 ms), "
        f"layout {layout_ms:.3f} ms, relayout (all cached) {relayout_ms:.3f} ms, paint {paint_ms:.3f} ms"
    )
    assert best < 20.0  # generous sanity bound; the real number is printed


SCROLL = 13


def test_scroll_container_wheel_hover_and_reveal():
    core = Core(200.0, 100.0)
    # mod 1: the 200x100 viewport; 2: a 200x50 clickable row (handler 1) hovering blue;
    # 3/4: rows with handlers 2 and 3; 5: handler 3 plus `reveal`
    mods = [
        (1, [2.0, 200.0, 3.0, 100.0]),
        (2, [2.0, 200.0, 3.0, 50.0, 9.0, 1.0, 12.0, float(0xFF0000FF)]),
        (3, [2.0, 200.0, 3.0, 50.0, 9.0, 2.0]),
        (4, [2.0, 200.0, 3.0, 50.0, 9.0, 3.0]),
        (5, [2.0, 200.0, 3.0, 50.0, 9.0, 3.0, 14.0]),
    ]
    rows = [rec(SCROLL, modifier=1), rec(SPACER, modifier=2), rec(SPACER, modifier=3), rec(SPACER, modifier=4), END]
    commit(core, rows, mods=mods)
    core.layout()
    core.paint()
    found = core.first_scroll()
    assert found is not None
    node_id, offset, content, viewport = found
    assert (offset, content, viewport) == (0.0, 150.0, 100.0)
    assert core.scroll_offset(node_id) == 0.0
    assert core.scroll_offset(123456789) is None

    assert core.hit_test(10, 75) == 2
    assert core.scroll(10, 75, 30) is True
    assert core.scroll_offset(node_id) == 30.0
    assert core.hit_test(10, 75) == 3, "the third row moved under the pointer"
    assert core.scroll(10, 75, 1000) and core.scroll_offset(node_id) == 50.0
    assert core.scroll(10, 75, 10) is False
    assert core.scroll(10, 75, -1000) and core.scroll_offset(node_id) == 0.0
    with pytest.raises(ValueError):
        core.scroll(10, 75, float("nan"))

    # hovering the first row paints its hover colour over it
    ev = core.pointer(3, 10.0, 10.0)
    assert ev[3] == 1 and core.hovered_handler() == 1
    core.paint()
    assert pixel(core.pixels(), 200, 5, 5) == (0, 0, 255, 255)
    core.pointer(3, 250.0, 150.0)
    assert core.hovered_handler() == -1, "nothing under a point outside the window"
    assert "scroll=0 content=150" in core.dump()

    # the third row gaining `reveal` scrolls it into view
    rows = [rec(SCROLL, modifier=1), rec(SPACER, modifier=2), rec(SPACER, modifier=3), rec(SPACER, modifier=5), END]
    commit(core, rows, mods=mods)
    core.layout()
    assert core.scroll_offset(node_id) == 50.0
    # a scroll container must be vertical
    with pytest.raises(ValueError):
        commit(core, [rec(SCROLL, a=1), END])


def test_style_flags_and_decorated_layers():
    core = Core(300.0, 100.0)
    styles = [(1, 14.0, float(0xFF000000), 7)]
    # rounded 8, white background, 1px black border, shadow 2, then padding 8
    mods = [(1, [10.0, 8.0, 6.0, float(0xFFFFFFFF), 11.0, 1.0, float(0xFF000000), 13.0, 2.0, float(0x40000000), 1.0, 8.0, 8.0, 8.0, 8.0, 15.0])]
    records = [rec(COLUMN, modifier=1), rec(TEXT, text=0, a=1), END]
    commit(core, records, ["mono bold nowrap"], mods=mods, styles=styles)
    core.layout()
    core.paint()
    assert core.find_text("mono bold nowrap") is not None
    assert "clip" in core.dump()
    with pytest.raises(ValueError):
        commit(core, records, ["mono bold nowrap"], styles=[(2, 14.0, 0.0, 8)])
    with pytest.raises(ValueError):
        commit(core, records, ["x"], mods=[(9, [10.0, -1.0])])
