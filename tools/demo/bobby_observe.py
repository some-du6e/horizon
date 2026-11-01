#!/usr/bin/env python3
"""Run Bobby (the Fabric mod, LGPL-3.0) as a player would and keep what it writes to disk, so its cache
format can be described from its files alone. Nobody reads Bobby's source: CONTRIBUTING allows running a
mod and observing it, whatever its license. Fork-only CI (bobby-observe.yml); never part of a PR.

Three servers, one client game folder (the cache builds up across them):
  A  vanilla 26.3 on port 25565, seed 1111, joined as "127.0.0.1"
  B  port 25566, seed 2222, joined as "localhost:25566"
  C  port 25565 again, seed 3333 (another world, same address as A), joined as "127.0.0.1"

    python3 tools/demo/bobby_observe.py MINECRAFT_DIR MODS_DIR OUT_DIR
"""
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

FABRIC_LOADER = '0.20.1'
POSITIONS = [(0, 0), (480, 0), (960, 0), (960, 480), (480, 960), (0, 480), (-600, -600)]
STAY = 20


def fetch_json(url: str):
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def fabric_libraries(version: str, libs: Path) -> tuple[list[Path], str]:
    profile = fetch_json(f'https://meta.fabricmc.net/v2/versions/loader/{version}/{FABRIC_LOADER}/profile/json')
    paths = []
    for library in profile['libraries']:
        group, artifact, ver = library['name'].split(':')[:3]
        rel = f'{group.replace(".", "/")}/{artifact}/{ver}/{artifact}-{ver}.jar'
        path = libs / rel
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(library['url'].rstrip('/') + '/' + rel, timeout=60) as response:
                path.write_bytes(response.read())
        paths.append(path)
    return paths, profile['mainClass']


def vanilla_classpath(mc: Path, version: str) -> tuple[list[Path], dict]:
    sys.path.insert(0, str(Path(__file__).parent))
    from minecraft import allowed  # noqa: E402

    manifest = json.loads((mc / 'versions' / version / f'{version}.json').read_text())
    paths = []
    for library in manifest['libraries']:
        artifact = library.get('downloads', {}).get('artifact')
        if artifact and allowed(library):
            paths.append(mc / 'libraries' / artifact['path'])
    return paths, manifest


class Proc:
    def __init__(self, name: str, args: list[str], cwd: Path, log: Path, env=None):
        self.lines: list[str] = []
        self.log = log.open('a')
        self.proc = subprocess.Popen(args, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, text=True, env=env)
        self.name = name
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        for line in self.proc.stdout:
            self.lines.append(line)
            self.log.write(line)
            self.log.flush()

    def wait_for(self, text: str, timeout: float) -> bool:
        end = time.time() + timeout
        seen = 0
        while time.time() < end:
            while seen < len(self.lines):
                if text in self.lines[seen]:
                    return True
                seen += 1
            if self.proc.poll() is not None:
                return False
            time.sleep(0.5)
        return False

    def send(self, command: str):
        print(f'[{self.name}] > {command}', flush=True)
        self.proc.stdin.write(command + '\n')
        self.proc.stdin.flush()

    def stop(self, timeout: float = 60):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def screenshot(out: Path, name: str):
    subprocess.run(['ffmpeg', '-loglevel', 'error', '-y', '-f', 'x11grab', '-i', os.environ.get('DISPLAY', ':99'),
                    '-frames:v', '1', str(out / f'{name}.png')], check=False)


def scenario(label, port, seed, address, mc, server_jar, game_dir, launch, out):
    server_dir = out / f'server-{label}'
    server_dir.mkdir(parents=True)
    (server_dir / 'eula.txt').write_text('eula=true\n')
    (server_dir / 'server.properties').write_text('\n'.join([
        'online-mode=false', 'white-list=false', 'enforce-whitelist=false', 'enforce-secure-profile=false', f'server-port={port}', f'level-seed={seed}',
        'view-distance=4', 'simulation-distance=4', 'spawn-protection=0', 'spawn-monsters=false', 'motd=observe',
    ]) + '\n')
    server = Proc(f'server-{label}', ['java', '-Xmx2G', '-jar', str(server_jar), 'nogui'], server_dir,
                  out / f'server-{label}.log')
    if not server.wait_for('Done (', 600):
        raise SystemExit(f'server {label} did not start')
    client = Proc(f'client-{label}', launch + ['--quickPlayMultiplayer', address], game_dir, out / f'client-{label}.log')
    if not server.wait_for('Bot joined the game', 600):
        screenshot(out, f'{label}-no-join')
        client.stop()
        server.stop()
        raise SystemExit(f'client never joined server {label}')
    time.sleep(10)
    server.send('gamemode spectator Bot')
    for i, (x, z) in enumerate(POSITIONS):
        server.send(f'execute in minecraft:overworld run tp Bot {x} 140 {z}')
        time.sleep(STAY)
        if i == 1:
            screenshot(out, f'{label}-overworld')
    if label == 'A':
        for x, z in [(0, 0), (300, 0)]:
            server.send(f'execute in minecraft:the_nether run tp Bot {x} 100 {z}')
            time.sleep(STAY)
        screenshot(out, f'{label}-nether')
        server.send('execute in minecraft:overworld run tp Bot 0 140 0')
        time.sleep(STAY)
    server.send('kick Bot observed')
    time.sleep(20)
    screenshot(out, f'{label}-after-kick')
    client.stop()
    server.send('stop')
    server.proc.wait(120)
    shutil.copytree(game_dir / '.bobby', out / f'bobby-after-{label}', dirs_exist_ok=True) \
        if (game_dir / '.bobby').exists() else None


def main():
    mc, mods, out = (Path(a).resolve() for a in sys.argv[1:4])
    out.mkdir(parents=True, exist_ok=True)
    version = json.loads((Path(__file__).resolve().parents[2] / 'research-lock.json').read_text())['minecraft']['version']
    game_dir = out.parent / 'game'
    (game_dir / 'mods').mkdir(parents=True, exist_ok=True)
    for jar in mods.glob('*.jar'):
        shutil.copy(jar, game_dir / 'mods' / jar.name)
    (game_dir / 'options.txt').write_text('\n'.join([
        'renderDistance:12', 'pauseOnLostFocus:false', 'onboardAccessibility:false', 'narrator:0',
        'tutorialStep:none', 'joinedFirstServer:true', 'skipMultiplayerWarning:true', 'soundCategory_master:0.0',
    ]) + '\n')

    vanilla, manifest = vanilla_classpath(mc, version)
    fabric, main_class = fabric_libraries(version, mc / 'libraries')
    fabric_names = {p.parent.parent.name for p in fabric}
    classpath = fabric + [p for p in vanilla if p.parent.parent.name not in fabric_names]
    classpath.append(mc / 'versions' / version / f'{version}.jar')
    launch = [
        'java', '-Xmx3G', '-cp', ':'.join(map(str, classpath)), main_class,
        '--username', 'Bot', '--version', f'fabric-{version}', '--gameDir', str(game_dir),
        '--assetsDir', str(mc / 'assets'), '--assetIndex', manifest['assetIndex']['id'],
        '--uuid', '00000000000000000000000000000b0b', '--accessToken', '0', '--userType', 'legacy',
        '--versionType', 'release', '--width', '1280', '--height', '720',
    ]
    server_jar = mc / 'server.jar'
    failures = []
    for label, port, seed, address in [('A', 25565, 1111, '127.0.0.1'), ('B', 25566, 2222, 'localhost:25566'),
                                       ('C', 25565, 3333, '127.0.0.1')]:
        try:
            scenario(label, port, seed, address, mc, server_jar, game_dir, launch, out)
        except SystemExit as error:
            failures.append(str(error))
            print(error, flush=True)
    for name in ['config', 'logs']:
        if (game_dir / name).exists():
            shutil.copytree(game_dir / name, out / f'game-{name}', dirs_exist_ok=True)
    if (game_dir / '.bobby').exists():
        shutil.copytree(game_dir / '.bobby', out / 'bobby', dirs_exist_ok=True)
    if (out / 'server-A' / 'world' / 'region').exists():
        shutil.copytree(out / 'server-A' / 'world' / 'region', out / 'server-A-region')
    for server in out.glob('server-?'):
        if server.is_dir():
            shutil.rmtree(server)
    if failures:
        raise SystemExit('; '.join(failures))


if __name__ == '__main__':
    main()
