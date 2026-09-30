#!/usr/bin/env python3
"""Generate path-only SVG mascots from the original PNG artwork."""
import argparse
from pathlib import Path
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "assets/sippy"
SOURCES = {
    "logo": (ROOT / "assets/logo/sippy-logo.png", "Sippy"),
    "ready": (OUTPUT / "sippy-tilted.png", "Sippy ready"),
    "on-call": (OUTPUT / "sippy-on-call.png", "Sippy on a call"),
    "dnd": (OUTPUT / "sippy-dnd.png", "Sippy do not disturb"),
    "not-registered": (OUTPUT / "sippy-not-registered.png", "Sippy not registered"),
}
SVG_NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG_NS)


def trace(vtracer, source, target, title, temporary):
    with Image.open(source) as original:
        image = original.convert("RGBA").resize((512, 512), Image.Resampling.LANCZOS)
    alpha = image.getchannel("A").point(lambda value: 255 if value >= 128 else 0)
    # Transparent pixels must not consume palette entries or tint the white face.
    palette_image = Image.new("RGBA", image.size, "white")
    palette_image.alpha_composite(image)
    image = palette_image.convert("RGB").quantize(colors=8).convert("RGBA")
    image.putalpha(alpha)
    raster = temporary / "source.png"
    vector = temporary / "traced.svg"
    image.save(raster)
    subprocess.run([
        vtracer, "--input", str(raster), "--output", str(vector),
        "--colormode", "color", "--hierarchical", "stacked", "--mode", "spline",
        "--filter_speckle", "4", "--color_precision", "5", "--gradient_step", "24",
        "--corner_threshold", "60", "--segment_length", "4",
        "--splice_threshold", "45", "--path_precision", "2",
    ], check=True)
    root = ET.parse(vector).getroot()
    root.set("viewBox", "0 0 512 512")
    label = ET.Element(f"{{{SVG_NS}}}title")
    label.text = title
    root.insert(0, label)
    ET.indent(root)
    ET.ElementTree(root).write(target, encoding="unicode", xml_declaration=False)
    with target.open("a") as output:
        output.write("\n")
    print(f"{target.relative_to(ROOT)}: {target.stat().st_size} bytes", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vtracer", default="vtracer", help="VTracer executable (0.6.5)")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="sippy-trace-") as directory:
        for name, (source, title) in SOURCES.items():
            trace(args.vtracer, source, OUTPUT / f"{name}.svg", title, Path(directory))


if __name__ == "__main__":
    main()
