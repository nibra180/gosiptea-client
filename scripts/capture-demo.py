#!/usr/bin/env python3
"""Capture the offline demo window with its frame and surrounding wallpaper."""
import json
import os
from pathlib import Path
import subprocess
import time
from zipfile import ZIP_DEFLATED, ZipFile

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "docs/screenshots"
MARGIN = 40
env = os.environ.copy()
runtime = Path(f"/run/user/{os.getuid()}")
instances = list((runtime / "hypr").iterdir())
assert len(instances) == 1, "Select a Hyprland instance explicitly"
env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY="wayland-1",
           HYPRLAND_INSTANCE_SIGNATURE=instances[0].name)


def hypr(*args):
    return subprocess.check_output(["hyprctl", *args], env=env, text=True)


def clients():
    return json.loads(hypr("clients", "-j"))


def dispatch(name, arg):
    if name == "focuswindow":
        code = f'hl.dsp.focus({{ window = "{arg}" }})'
    elif name == "workspace":
        code = f'hl.dsp.focus({{ workspace = "{arg}" }})'
    elif name == "setfloating":
        code = f'hl.dsp.window.float({{ window = "{arg}", action = "set" }})'
    elif name in ("resizewindowpixel", "movewindowpixel"):
        coordinates, window = arg.split(",")
        _, x, y = coordinates.split()
        operation = "resize" if name == "resizewindowpixel" else "move"
        code = f'hl.dsp.window.{operation}({{ window = "{window}", x = {x}, y = {y} }})'
    elif name == "sendshortcut":
        mods, key, window = arg.split(",")
        code = f'hl.dsp.send_shortcut({{ mods = "{mods}", key = "{key}", window = "{window}" }})'
    else:
        raise ValueError(name)
    result = hypr("dispatch", code)
    assert result.strip() == "ok", result


def capture(client, filename):
    address = client["address"]
    dispatch("focuswindow", f"address:{address}")
    time.sleep(0.6)
    active = json.loads(hypr("activewindow", "-j"))
    assert active["address"] == address and active["class"] == "gosiptea-demo"
    x, y = active["at"]
    width, height = active["size"]
    assert active["workspace"]["id"] == demo_workspace
    assert all(c["address"] == address for c in clients()
               if c["workspace"]["id"] == demo_workspace or c.get("pinned")), "Another window overlaps the demo workspace"
    x, y = x - MARGIN, y - MARGIN
    width, height = width + 2 * MARGIN, height + 2 * MARGIN
    subprocess.run(["grim", "-g", f"{x},{y} {width}x{height}", str(OUTPUT / filename)],
                   env=env, check=True)
    print(f"{filename}: {width}x{height}", flush=True)


OUTPUT.mkdir(parents=True, exist_ok=True)
previous = json.loads(hypr("activewindow", "-j")).get("address")
previous_workspace = json.loads(hypr("activeworkspace", "-j"))["id"]
previous_cursor = json.loads(hypr("cursorpos", "-j"))
monitor = next(m for m in json.loads(hypr("monitors", "-j")) if m["focused"])
used_workspaces = {w["id"] for w in json.loads(hypr("workspaces", "-j"))}
demo_workspace = next(i for i in range(9000, 10000) if i not in used_workspaces)
try:
    dispatch("workspace", str(demo_workspace))
    result = hypr("dispatch", f'hl.dsp.cursor.move({{ x = {monitor["x"] + 10}, y = {monitor["y"] + 10} }})')
    assert result.strip() == "ok", result
    for incoming in (False, True):
        args = [str(ROOT / "target/debug/examples/demo_screenshots")]
        if incoming:
            args.append("--incoming")
        with open(OUTPUT / "capture.log", "a") as log:
            process = subprocess.Popen(args, env=env, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    matches = [c for c in clients() if c["pid"] == process.pid and c["class"] == "gosiptea-demo"]
                    if matches:
                        break
                    assert process.poll() is None, "Demo process exited; inspect capture.log"
                    time.sleep(0.1)
                assert matches, "Demo window did not appear"
                client = matches[0]
                selector = f"address:{client['address']}"
                dispatch("setfloating", selector)
                for prop in ("opaque", "opacity", "opacity_override", "force_rgbx"):
                    result = hypr("dispatch", f'hl.dsp.window.set_prop({{ window = "{selector}", prop = "{prop}", value = "1" }})')
                    assert result.strip() == "ok", result
                for viewport, width, height in [("desktop", 1100, 760), ("compact", 600, 800)]:
                    dispatch("resizewindowpixel", f"exact {width} {height},{selector}")
                    x = monitor["x"] + round((monitor["width"] / monitor["scale"] - width) / 2)
                    y = monitor["y"] + round((monitor["height"] / monitor["scale"] - height) / 2)
                    dispatch("movewindowpixel", f"exact {x} {y},{selector}")
                    time.sleep(0.5)
                    screens = [(1, "incoming")] if incoming else [(1, "phone"), (2, "contacts"), (3, "account"), (4, "history"), (5, "settings")]
                    for key, screen in screens:
                        dispatch("sendshortcut", f"CTRL,{key},{selector}")
                        capture(client, f"{viewport}-{screen}.png")
            finally:
                process.terminate()
                process.wait(timeout=10)
finally:
    dispatch("workspace", str(previous_workspace))
    hypr("dispatch", f'hl.dsp.cursor.move({{ x = {previous_cursor["x"]}, y = {previous_cursor["y"]} }})')
    if previous and any(c["address"] == previous for c in clients()):
        dispatch("focuswindow", f"address:{previous}")

with ZipFile(ROOT / "docs/demo-screenshots.zip", "w", ZIP_DEFLATED) as archive:
    for path in sorted(OUTPUT.glob("*.png")) + [OUTPUT / "README.md"]:
        archive.write(path, "gosiptea-demo/" + path.name)
log = OUTPUT / "capture.log"
if log.exists() and not log.stat().st_size:
    log.unlink()
