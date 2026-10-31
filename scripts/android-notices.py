#!/usr/bin/env python3
"""Bundle local upstream license metadata/texts for the resolved Android dependency graph."""
import argparse, hashlib, io, json, os, subprocess, xml.etree.ElementTree as ET, zipfile
from pathlib import Path

p=argparse.ArgumentParser();p.add_argument('--artifacts',required=True);p.add_argument('--gradle-home',required=True);p.add_argument('--output',required=True);args=p.parse_args()
root=Path(__file__).resolve().parent.parent
# A fresh CI checkout may still need manifests for workspace dependencies that
# the native build did not compile. Let Cargo fetch the locked graph; actual
# license texts are read from those local crates, never scraped from web pages.
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--format-version','1','--filter-platform','aarch64-linux-android'],cwd=root))
packages={p['id']:p for p in metadata['packages']};nodes={n['id']:n for n in metadata['resolve']['nodes']}
start=next(p['id'] for p in metadata['packages'] if p['name']=='torto-android-bridge')
seen=set();queue=[start]
while queue:
    key=queue.pop()
    if key in seen:continue
    seen.add(key);queue.extend(d['pkg'] for d in nodes[key]['deps'] if any(k['kind']!='dev' for k in d['dep_kinds']))
texts={};rows=[]
def store(data):
    if not data or len(data)>128*1024:return None
    text=data.decode('utf-8',errors='replace').strip()
    if not text:return None
    key=hashlib.sha256(text.encode()).hexdigest();texts[key]=text;return key

def license_files(folder):
    found=[]
    for file in sorted(folder.iterdir()):
        if file.is_file() and file.name.upper().startswith(('LICENSE','LICENCE','COPYING','NOTICE')):
            key=store(file.read_bytes())
            if key:found.append(key)
    return found
for key in sorted(seen):
    pkg=packages[key];folder=Path(pkg['manifest_path']).parent
    files=license_files(folder)
    if pkg.get('license_file'):
        file=folder/pkg['license_file']
        if file.is_file():
            value=store(file.read_bytes())
            if value:files.append(value)
    rows.append(dict(name=pkg['name'],version=pkg['version'],license=pkg.get('license') or 'See upstream license',url=pkg.get('repository') or pkg.get('homepage') or '',texts=list(dict.fromkeys(files)),ecosystem='Rust'))
cache=Path(args.gradle_home)/'caches/modules-2/files-2.1'
def archive_texts(archive):
    found=[]
    for info in archive.infolist():
        name=info.filename.upper().split('/')[-1]
        if info.file_size<=128*1024 and name.startswith(('LICENSE','LICENCE','COPYING','NOTICE')):
            value=store(archive.read(info))
            if value:found.append(value)
    return found
for artifact in json.loads(Path(args.artifacts).read_text()):
    group,name,version=(artifact[k] for k in ('group','name','version'));licenses=[];url='';files=[]
    for pom in sorted((cache/group/name/version).glob('*/*.pom')):
        xml=ET.parse(pom).getroot()
        for node in xml.iter():node.tag=node.tag.split('}')[-1]
        licenses=[n.text for n in xml.findall('./licenses/license/name') if n.text]
        url=xml.findtext('./url') or xml.findtext('./scm/url') or ''
        if not url:
            url=next((n.text for n in xml.findall('./licenses/license/url') if n.text),'')
        break
    file=Path(artifact['file'])
    if zipfile.is_zipfile(file):
        with zipfile.ZipFile(file) as archive:
            files+=archive_texts(archive)
            if 'classes.jar' in archive.namelist() and archive.getinfo('classes.jar').file_size<32*1024*1024:
                with zipfile.ZipFile(io.BytesIO(archive.read('classes.jar'))) as classes:files+=archive_texts(classes)
    rows.append(dict(name=group+':'+name,version=version,license=' / '.join(licenses) or 'See upstream license',url=url,texts=list(dict.fromkeys(files)),ecosystem='Android'))
rows.insert(0,dict(name='Torto',version=next(p['version'] for p in packages.values() if p['name']=='torto-android-bridge'),license='AGPL-3.0-only',url='https://github.com/TortoTech/torto',texts=[store((root/'LICENSE').read_bytes())],ecosystem='Application'))
rows=sorted(rows,key=lambda p:(p['ecosystem']!='Application',p['ecosystem'],p['name'].lower()))
output=Path(args.output);output.parent.mkdir(parents=True,exist_ok=True)
output.write_text(json.dumps(dict(packages=rows,texts=texts),ensure_ascii=False,separators=(',',':')),encoding='utf-8')
print(f'Bundled {len(rows)} dependency notices, {len(texts)} upstream license texts')
