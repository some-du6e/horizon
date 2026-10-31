#!/usr/bin/env python3
"""Download the pinned Minecraft (research-lock.json) from Mojang into a game directory laid out as
the official launcher's, for horizon-prepare on a CI runner that has no copy of its own: the version
JSON, the client jar, its libraries, the asset index and objects, and the server jar beside them.

Every file is checked against the SHA-1 Mojang publishes for it. Nothing here is kept or uploaded:
the runner's copy goes with the runner. Only for the demo workflows.

    python3 tools/demo/minecraft.py OUT_DIR
"""
import concurrent.futures
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RESOURCES = 'https://resources.download.minecraft.net'


def fetch(url: str) -> bytes:
    for attempt in range(4):
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                return response.read()
        except OSError:
            if attempt == 3:
                raise
    raise AssertionError


def save(url: str, sha1: str, path: Path) -> None:
    if path.exists() and hashlib.sha1(path.read_bytes()).hexdigest() == sha1:
        return
    data = fetch(url)
    got = hashlib.sha1(data).hexdigest()
    if got != sha1:
        raise SystemExit(f'{url}: sha1 {got}, expected {sha1}')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def allowed(library: dict) -> bool:
    """The launcher's rules: the last rule that applies to Linux decides; none means allowed."""
    rules = library.get('rules')
    if not rules:
        return True
    allow = False
    for rule in rules:
        os = rule.get('os')
        if os is None or os.get('name') == 'linux':
            allow = rule['action'] == 'allow'
    return allow


def main() -> None:
    out = Path(sys.argv[1]).resolve()
    pin = json.loads((ROOT / 'research-lock.json').read_text())['minecraft']
    version = pin['version']
    manifest_url = f'https://piston-meta.mojang.com/v1/packages/{pin["manifest_sha1"]}/{version}.json'
    manifest_path = out / 'versions' / version / f'{version}.json'
    save(manifest_url, pin['manifest_sha1'], manifest_path)
    manifest = json.loads(manifest_path.read_text())
    downloads = manifest['downloads']
    jobs = [
        (downloads['client']['url'], pin['client_sha1'], out / 'versions' / version / f'{version}.jar'),
        (downloads['server']['url'], pin['server_sha1'], out / 'server.jar'),
    ]
    for library in manifest['libraries']:
        artifact = library.get('downloads', {}).get('artifact')
        if artifact and allowed(library):
            jobs.append((artifact['url'], artifact['sha1'], out / 'libraries' / artifact['path']))
    index = manifest['assetIndex']
    index_path = out / 'assets' / 'indexes' / f'{index["id"]}.json'
    save(index['url'], index['sha1'], index_path)
    for entry in json.loads(index_path.read_text())['objects'].values():
        digest = entry['hash']
        jobs.append((f'{RESOURCES}/{digest[:2]}/{digest}', digest, out / 'assets' / 'objects' / digest[:2] / digest))
    with concurrent.futures.ThreadPoolExecutor(16) as pool:
        for future in concurrent.futures.as_completed([pool.submit(save, *job) for job in jobs]):
            future.result()
    print(f'Minecraft {version}: {len(jobs)} files in {out}')


if __name__ == '__main__':
    main()
