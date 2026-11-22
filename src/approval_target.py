"""Return the AT-SPI target for a desktop-scoped click or focused input."""
import ctypes as C
import json
import os
from pathlib import Path
import sys

try:
    lib = C.CDLL(os.environ["SELLAITE_ATSPI_LIBRARY"])
except OSError as error:
    print(error, file=sys.stderr)
    sys.exit(78)
ptr = C.c_void_p
integer = C.c_int


class Rect(C.Structure):
    _fields_ = [(field, integer) for field in ("X", "Y", "Width", "Height")]


def bind(name, result, *args):
    fn = getattr(lib, "atspi_" + name)
    fn.restype = result
    fn.argtypes = args
    return fn


init = bind("init", integer)
desktop = bind("get_desktop", ptr, integer)
count = bind("accessible_get_child_count", integer, ptr, ptr)
child = bind("accessible_get_child_at_index", ptr, ptr, integer, ptr)
pid = bind("accessible_get_process_id", C.c_uint, ptr, ptr)
role = bind("accessible_get_role_name", C.c_char_p, ptr, ptr)
name = bind("accessible_get_name", C.c_char_p, ptr, ptr)
states = bind("accessible_get_state_set", ptr, ptr)
contains = bind("state_set_contains", integer, ptr, integer)
component = bind("accessible_get_component_iface", ptr, ptr)
extents = bind("component_get_extents", C.POINTER(Rect), ptr, integer, ptr)
hit = bind("component_get_accessible_at_point", ptr, ptr, integer, integer, integer, ptr)
FOCUSED = 12
SHOWING = 25
VISIBLE = 30


def children(node):
    for i in range(count(node, None)):
        item = child(node, i, None)
        if item:
            yield item


def showing(node):
    state = states(node)
    return state and all(contains(state, flag) for flag in (SHOWING, VISIBLE))


def focused(window):
    todo = list(children(window))
    while todo:
        node = todo.pop()
        state = states(node)
        if state and contains(state, FOCUSED) and showing(node):
            yield node
        todo.extend(children(node))


def hit_target(node, x, y):
    result = None
    while comp := component(node):
        nested = hit(comp, x, y, 0, None)  # ATSPI_COORD_TYPE_SCREEN
        if not nested or not showing(nested):
            break
        result = nested
        if nested == node:
            break
        node = nested
    return result


def at_point(window, x, y):
    node = hit_target(window, x, y)
    if node and node != window:
        return {node}
    return {
        node for child in children(window)
        if showing(child) and (node := hit_target(child, x, y))
    }


def label(node):
    return (name(node, None) or b"").decode("utf-8", "replace")


def target(node):
    result = {
        "Role": (role(node, None) or b"").decode("utf-8", "replace"),
        "Name": label(node),
    }
    comp = component(node)
    rect = extents(comp, 0, None) if comp else None
    if rect:
        r = rect.contents
        if r.Width > 0 and r.Height > 0 and (
            (r.X <= point[0] < r.X + r.Width and r.Y <= point[1] < r.Y + r.Height)
            if point else (
                r.X < box["x"] + box["width"] and box["x"] < r.X + r.Width
                and r.Y < box["y"] + box["height"] and box["y"] < r.Y + r.Height
            )
        ):
            result["Bounds"] = {field: getattr(r, field) for field, _ in Rect._fields_}
    return result


if init() != 0:
    sys.exit(1)
root = desktop(0)
if not root:
    sys.exit(1)
marker = ("SELLAITE_DESKTOP=" + sys.argv[1]).encode()
point = [int(value) for value in sys.argv[2].split(",")] if sys.argv[2] else None
owner, title = int(sys.argv[3]), sys.argv[4]
box = json.loads(sys.argv[5])
try:
    environ = Path(f"/proc/{owner}/environ").read_bytes().split(b"\0")
except OSError:
    environ = []
windows = [
    window
    for app in children(root) if pid(app, None) == owner
    for window in children(app) if label(window) == title
] if marker in environ else []
nodes = []
if len(windows) == 1 and showing(window := windows[0]):
    nodes = list(at_point(window, *point) if point else focused(window))
print(json.dumps(target(nodes[0]) if len(nodes) == 1 else None))
