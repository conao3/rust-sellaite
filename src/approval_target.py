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


def children(node):
    for i in range(count(node, None)):
        item = child(node, i, None)
        if item:
            yield item


def focused(app):
    todo = [app]
    while todo:
        node = todo.pop()
        state = states(node)
        if state and contains(state, 12):  # ATSPI_STATE_FOCUSED
            yield node
        todo.extend(children(node))


def at_point(app, x, y):
    for window in children(app):
        node = window
        while node and (comp := component(node)):
            nested = hit(comp, x, y, 0, None)  # ATSPI_COORD_TYPE_SCREEN
            if not nested or nested == node:
                break
            node = nested
        if node != window:
            yield node


def target(node):
    result = {
        "Role": (role(node, None) or b"").decode("utf-8", "replace"),
        "Name": (name(node, None) or b"").decode("utf-8", "replace"),
    }
    comp = component(node)
    rect = extents(comp, 0, None) if comp else None
    if rect and rect.contents.Width > 0 and rect.contents.Height > 0:
        result["Bounds"] = {field: getattr(rect.contents, field) for field, _ in Rect._fields_}
    return result


if init() != 0:
    sys.exit(1)
root = desktop(0)
if not root:
    sys.exit(1)
marker = ("SELLAITE_DESKTOP=" + sys.argv[1]).encode()
point = [int(value) for value in sys.argv[2].split(",")] if sys.argv[2] else None
targets = []
for app in children(root):
    try:
        environ = Path(f"/proc/{pid(app, None)}/environ").read_bytes().split(b"\0")
    except OSError:
        continue
    if marker not in environ:
        continue
    targets.extend(map(target, at_point(app, *point) if point else focused(app)))
print(json.dumps(targets[0] if len(targets) == 1 else None))
