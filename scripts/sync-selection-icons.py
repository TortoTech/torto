#!/usr/bin/env python3
"""Generate Android selection vectors from the SVG assets also used by desktop."""
from pathlib import Path
import argparse
import xml.etree.ElementTree as ET

root=Path(__file__).resolve().parent.parent
parser=argparse.ArgumentParser();parser.add_argument('--check',action='store_true');args=parser.parse_args()
android='http://schemas.android.com/apk/res/android';ET.register_namespace('android',android)
mapping={'d':'pathData','fill':'fillColor','stroke':'strokeColor','stroke-width':'strokeWidth','stroke-linecap':'strokeLineCap','stroke-linejoin':'strokeLineJoin'}
for name in ['copy','underline','thought','remove-underline','ai']:
    source=root/'assets/reader-icons'/f'{name}.svg';svg=ET.parse(source).getroot();box=svg.attrib['viewBox'].split()
    vector=ET.Element('vector',{f'{{{android}}}{k}':v for k,v in {'width':'24dp','height':'24dp','viewportWidth':box[2],'viewportHeight':box[3]}.items()})
    def convert(node,parent):
        for child in node:
            tag=child.tag.rsplit('}',1)[-1]
            if tag=='g':
                transform=child.attrib.get('transform','translate(0 0)');assert transform.startswith('translate(')
                x,y=transform[len('translate('):-1].replace(',',' ').split()
                group=ET.SubElement(parent,'group',{f'{{{android}}}translateX':x,f'{{{android}}}translateY':y});convert(child,group)
            elif tag=='path':
                values={f'{{{android}}}{target}':child.attrib[key] for key,target in mapping.items() if key in child.attrib}
                for key in ['fillColor','strokeColor']:
                    attr=f'{{{android}}}{key}'
                    if values.get(attr)=='none':values[attr]='@android:color/transparent'
                    if values.get(attr)=='#ffffff':values[attr]='@android:color/white'
                ET.SubElement(parent,'path',values)
    convert(svg,vector);ET.indent(vector,space='    ')
    text=f'<!-- Generated from assets/reader-icons/{name}.svg by scripts/sync-selection-icons.py. -->\n'+ET.tostring(vector,encoding='unicode')+'\n'
    target=root/'apps/android/app/src/main/res/drawable'/f'ic_symbol_{name.replace("-","_")}.xml'
    if args.check:
        if not target.exists() or target.read_text()!=text:raise SystemExit(f'Out of date: {target}')
    else:target.write_text(text)
print('Selection icon assets match.' if args.check else 'Updated selection icon vectors.')
