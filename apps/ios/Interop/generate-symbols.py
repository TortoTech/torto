"""Build template SVG images from the same vectors Android displays."""
import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

source, destination = map(Path, sys.argv[1:])
android = "{http://schemas.android.com/apk/res/android}"
svg = "http://www.w3.org/2000/svg"
ET.register_namespace("", svg)


def get(node, attribute, default=None):
    return node.get(android + attribute, default)


def add_children(source, parent):
    for node in source:
        if node.tag == "group":
            group = ET.SubElement(parent, f"{{{svg}}}g", {
                "transform": f"translate({get(node, 'translateX', '0')} {get(node, 'translateY', '0')})",
            })
            add_children(node, group)
        elif node.tag == "path":
            attrs = {"d": get(node, "pathData"),
                     "fill": "none" if get(node, "fillColor") == "@android:color/transparent" else "#000"}
            if get(node, "strokeWidth"):
                attrs.update({"stroke": "#000", "stroke-width": get(node, "strokeWidth"),
                              "stroke-linecap": get(node, "strokeLineCap", "butt"),
                              "stroke-linejoin": get(node, "strokeLineJoin", "miter")})
            ET.SubElement(parent, f"{{{svg}}}path", attrs)
        else:
            raise ValueError(f"Unsupported vector element: {node.tag}")


for file in sorted(source.glob("ic_symbol_*.xml")):
    vector = ET.parse(file).getroot()
    image = ET.Element(f"{{{svg}}}svg", {"width": "24", "height": "24",
                       "viewBox": f"0 0 {get(vector, 'viewportWidth')} {get(vector, 'viewportHeight')}"})
    add_children(vector, image)
    name = file.stem.removeprefix("ic_symbol_")
    output = destination / f"symbol-{name}.imageset"
    output.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(image).write(output / "Symbol.svg", encoding="utf-8")
    (output / "Contents.json").write_text(json.dumps({
        "images": [{"filename": "Symbol.svg", "idiom": "universal"}],
        "info": {"author": "xcode", "version": 1},
        "properties": {"preserves-vector-representation": True, "template-rendering-intent": "template"},
    }), encoding="utf-8")
