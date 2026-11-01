#!/usr/bin/env python3
"""Describe a folder Bobby wrote (bobby_observe.py's output) from its bytes alone: every file and its size,
for region files the chunks present, their compression and NBT structure, and for anything else its
first bytes. Fork-only CI.

    python3 tools/demo/bobby_dump.py BOBBY_DIR [SERVER_REGION_DIR] > dump.txt
"""
import gzip
import io
import struct
import sys
import zlib
from pathlib import Path

TAGS = {0: 'end', 1: 'byte', 2: 'short', 3: 'int', 4: 'long', 5: 'float', 6: 'double', 7: 'byte[]',
        8: 'string', 9: 'list', 10: 'compound', 11: 'int[]', 12: 'long[]'}


def read_payload(f, tag):
    if tag == 1:
        return struct.unpack('>b', f.read(1))[0]
    if tag == 2:
        return struct.unpack('>h', f.read(2))[0]
    if tag == 3:
        return struct.unpack('>i', f.read(4))[0]
    if tag == 4:
        return struct.unpack('>q', f.read(8))[0]
    if tag == 5:
        return struct.unpack('>f', f.read(4))[0]
    if tag == 6:
        return struct.unpack('>d', f.read(8))[0]
    if tag == 7:
        n = struct.unpack('>i', f.read(4))[0]
        return ('byte[]', n, f.read(n)[:8].hex())
    if tag == 8:
        n = struct.unpack('>H', f.read(2))[0]
        return f.read(n).decode('utf-8', 'replace')
    if tag == 9:
        inner, n = struct.unpack('>bi', f.read(5))
        return ('list', TAGS.get(inner), [read_payload(f, inner) for _ in range(n)])
    if tag == 10:
        out = {}
        while True:
            t = f.read(1)[0]
            if t == 0:
                return out
            n = struct.unpack('>H', f.read(2))[0]
            name = f.read(n).decode('utf-8', 'replace')
            out[name] = (TAGS[t], read_payload(f, t))
    if tag == 11:
        n = struct.unpack('>i', f.read(4))[0]
        return ('int[]', n, list(struct.unpack(f'>{n}i', f.read(4 * n)))[:8])
    if tag == 12:
        n = struct.unpack('>i', f.read(4))[0]
        f.read(8 * n)
        return ('long[]', n)
    raise ValueError(tag)


def read_nbt(data: bytes):
    f = io.BytesIO(data)
    tag = f.read(1)[0]
    n = struct.unpack('>H', f.read(2))[0]
    name = f.read(n).decode()
    return name, TAGS[tag], read_payload(f, tag)


def show(value, indent=0, depth=0):
    pad = '  ' * indent
    if isinstance(value, dict):
        for key, (tag, inner) in value.items():
            if isinstance(inner, dict):
                print(f'{pad}{key}: compound')
                show(inner, indent + 1, depth + 1)
            elif isinstance(inner, tuple) and inner[0] == 'list':
                print(f'{pad}{key}: list<{inner[1]}> x{len(inner[2])}')
                if inner[2] and depth < 4:
                    show(inner[2][0], indent + 1, depth + 1)
            else:
                text = repr(inner)
                print(f'{pad}{key}: {tag} = {text[:120]}')
    elif isinstance(value, tuple) and value[0] == 'list':
        print(f'{pad}list<{value[1]}> x{len(value[2])}')
    else:
        print(f'{pad}{repr(value)[:120]}')


def decompress(kind: int, data: bytes) -> bytes:
    if kind == 1:
        return gzip.decompress(data)
    if kind == 2:
        return zlib.decompress(data)
    if kind == 3:
        return data
    raise ValueError(f'compression {kind}')


def region(path: Path, detail: bool):
    data = path.read_bytes()
    print(f'  {len(data)} bytes, header {len(data[:8192])} bytes')
    chunks = []
    for i in range(1024):
        loc = struct.unpack('>I', data[i * 4:i * 4 + 4])[0]
        if not loc:
            continue
        offset, sectors = loc >> 8, loc & 0xff
        stamp = struct.unpack('>I', data[4096 + i * 4:4096 + i * 4 + 4])[0]
        start = offset * 4096
        length, kind = struct.unpack('>IB', data[start:start + 5])
        chunks.append((i % 32, i // 32, offset, sectors, stamp, length, kind, start))
    print(f'  {len(chunks)} chunks; local (x,z) present: {[(x, z) for x, z, *_ in chunks][:40]}')
    kinds = sorted({c[6] for c in chunks})
    stamps = sorted({c[4] for c in chunks})
    print(f'  compression bytes {kinds}; timestamps {stamps[:3]}..{stamps[-3:]} ({len(stamps)} distinct)')
    if detail and chunks:
        for x, z, offset, sectors, stamp, length, kind, start in chunks[:2]:
            print(f'  chunk local ({x},{z}) offset {offset} sectors {sectors} length {length} compression {kind}')
            body = data[start + 5:start + 4 + length]
            if kind & 0x80:
                print('    external .mcc file')
                continue
            name, tag, value = read_nbt(decompress(kind, body))
            print(f'    root "{name}" {tag}')
            show(value, 3)


def main():
    root = Path(sys.argv[1])
    print(f'== tree of {root.name}')
    for path in sorted(root.rglob('*')):
        rel = path.relative_to(root)
        print(f'{"d" if path.is_dir() else "f"} {rel}' + ('' if path.is_dir() else f'  ({path.stat().st_size} bytes)'))
    print()
    detailed = set()
    for path in sorted(root.rglob('*')):
        if not path.is_file():
            continue
        rel = path.relative_to(root)
        print(f'== {rel}')
        if path.suffix in ('.mca', '.mcr'):
            folder = path.parent
            region(path, folder not in detailed)
            detailed.add(folder)
        else:
            data = path.read_bytes()
            try:
                if data[:2] == b'\x1f\x8b':
                    name, tag, value = read_nbt(gzip.decompress(data))
                    print(f'  gzip NBT root "{name}" {tag}')
                    show(value, 2)
                    continue
                name, tag, value = read_nbt(data)
                print(f'  NBT root "{name}" {tag}')
                show(value, 2)
            except Exception:
                print(f'  first bytes: {data[:256]!r}')
    if len(sys.argv) > 2:
        server = Path(sys.argv[2])
        print(f'\n== server region files, for comparison ({server})')
        for path in sorted(server.glob('*.mca'))[:2]:
            print(f'-- {path.name}')
            region(path, True)


if __name__ == '__main__':
    main()
