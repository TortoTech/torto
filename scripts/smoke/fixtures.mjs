// Small, original test publications. No downloads or third-party book content.
import fs from 'node:fs';
import path from 'node:path';
import { deflateSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function zip(files) {
  const local = [], central = [];
  let offset = 0;
  for (const [name, value] of files) {
    const filename = Buffer.from(name), data = Buffer.from(value), crc = crc32(data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50); header.writeUInt16LE(20, 4);
    header.writeUInt32LE(crc, 14); header.writeUInt32LE(data.length, 18);
    header.writeUInt32LE(data.length, 22); header.writeUInt16LE(filename.length, 26);
    local.push(header, filename, data);
    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50); entry.writeUInt16LE(20, 4); entry.writeUInt16LE(20, 6);
    entry.writeUInt32LE(crc, 16); entry.writeUInt32LE(data.length, 20);
    entry.writeUInt32LE(data.length, 24); entry.writeUInt16LE(filename.length, 28);
    entry.writeUInt32LE(offset, 42); central.push(entry, filename);
    offset += header.length + filename.length + data.length;
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(files.length, 8); end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, directory, end]);
}
function png() {
  const chunk = (name, data) => {
    const bytes = Buffer.concat([Buffer.from(name), data]), head = Buffer.alloc(4), tail = Buffer.alloc(4);
    head.writeUInt32BE(data.length); tail.writeUInt32BE(crc32(bytes));
    return Buffer.concat([head, bytes, tail]);
  };
  const header = Buffer.alloc(13); header.writeUInt32BE(32); header.writeUInt32BE(32, 4); header[8] = 8; header[9] = 2;
  const pixels = Buffer.alloc(32 * (32 * 3 + 1));
  for (let y = 0; y < 32; y++) for (let x = 0; x < 32; x++) {
    const i = y * 97 + 1 + x * 3;
    pixels[i] = x * 8; pixels[i + 1] = y * 8; pixels[i + 2] = 100;
  }
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk('IHDR', header), chunk('IDAT', deflateSync(pixels)), chunk('IEND', Buffer.alloc(0))]);
}
export function createFixtures(directory) {
  fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, 'startup.epub'), zip([
    ['mimetype', 'application/epub+zip'],
    ['META-INF/container.xml', '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'],
    ['book.opf', '<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">torto-startup-test</dc:identifier><dc:title>Torto startup check</dc:title><dc:language>en</dc:language><meta property="dcterms:modified">2026-01-01T00:00:00Z</meta></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="body"/></spine></package>'],
    ['nav.xhtml', '<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Startup</a></li></ol></nav></body></html>'],
    ['body.xhtml', '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Startup</title></head><body><h1>Torto startup check</h1><p>Rendered text, <em>emphasis</em> and an image. 中文字体与阅读排版检查。</p><p><img src="image.png" alt="Test colors" width="32" height="32"/></p></body></html>'],
    ['image.png', png()],
  ]));
  const stream = 'BT /F1 24 Tf 50 730 Td (Torto PDF startup check) Tj 0 -40 Td /F1 14 Tf (Text and vector graphics must render.) Tj ET\n0.2 0.6 0.4 rg 50 580 180 60 re f\n';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    `<< /Length ${Buffer.byteLength(stream)} >>\nstream\n${stream}endstream`,
  ];
  let pdf = '%PDF-1.4\n', offsets = [0];
  objects.forEach((object, i) => { offsets.push(Buffer.byteLength(pdf)); pdf += `${i + 1} 0 obj\n${object}\nendobj\n`; });
  const xref = Buffer.byteLength(pdf);
  pdf += `xref\n0 6\n0000000000 65535 f \n${offsets.slice(1).map(offset => `${String(offset).padStart(10, '0')} 00000 n \n`).join('')}trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  fs.writeFileSync(path.join(directory, 'startup.pdf'), pdf);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) createFixtures(path.resolve(process.argv[2]));
