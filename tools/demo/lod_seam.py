#!/usr/bin/env python3
"""Film far terrain right past the render distance, for comparing two builds side by side.

`record`: a new local world (seed 1, render distance 12, LOD Distance 64 chunks, Full far detail),
the HUD hidden, the camera at y=140 above spawn looking a little down, or with `--biome`, above
the nearest such biome (`/locate biome`), 60 blocks over the spot it names if that is higher. A screenshot every two
seconds while the terrain loads, for `--seconds`, then one towards each of the four directions.
`compose`: recordings' frames side by side, labelled: a video of the loading and stills at
fixed times and of the four directions.

    python3 tools/demo/lod_seam.py record --game target/fast/horizon-client \\
        --ctl target/fast/horizon-ctl --resources .local/horizon-client/26.3 --out before
    python3 tools/demo/lod_seam.py compose before after --labels "nightly 1da354b" "main" --out cmp
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

STEP = 2
SETTLED = 0.95
STILLS = (10, 30, 60, 120, 180, 240, 360)
DIRECTIONS = (0, 90, 180, 270)


def thread_cpu(pid: int) -> dict:
    """CPU seconds so far of each of the process's threads, summed by thread name
    (llvmpipe-0..n as llvmpipe): where the far terrain's time goes."""
    tick = os.sysconf('SC_CLK_TCK')
    total = {}
    for task in Path(f'/proc/{pid}/task').glob('*'):
        try:
            stat = (task / 'stat').read_text()
        except OSError:
            continue
        name = stat[stat.index('(') + 1:stat.rindex(')')]
        fields = stat[stat.rindex(')') + 2:].split()
        name = re.sub(r'[-:]?\d+$', '', name)
        total[name] = round(total.get(name, 0) + (int(fields[11]) + int(fields[12])) / tick, 2)
    return dict(sorted(total.items(), key=lambda item: -item[1]))


def settled_at(timeline: list, target='target_tiles', done='sampled_target_tiles') -> float | None:
    """Seconds until the far terrain sampled SETTLED of its target tiles (or, with
    the detail keys, until SETTLED of its columns have their generated detail).
    The target is the last one seen: the detail's grows as tiles are drawn."""
    final = next((entry[target] for entry in reversed(timeline) if entry.get(target)), None)
    for entry in timeline:
        if final and (entry.get(done) or 0) >= SETTLED * final:
            return entry['seconds']
    return None


def record(args) -> None:
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    # Fresh folders for everything the game writes: no player data is touched.
    home = Path(tempfile.mkdtemp(prefix='horizon-demo-'))
    env = dict(os.environ, HORIZON_HOME=str(home), HORIZON_NO_AUDIO='1', HORIZON_TELEMETRY_URL='off',
               HORIZON_LAUNCHER_NO_SELF_UPDATE='1',
               XDG_DATA_HOME=tempfile.mkdtemp(), XDG_CONFIG_HOME=tempfile.mkdtemp())
    (home / 'horizon-graphics.txt').write_text('lodDistance:64\nfarDetail:full\n')
    session(args, out, home, env, args.seconds)
    if args.reopen:
        # The same world opened again: what the first session saved and cached.
        session(args, out / 'reopen', home, env, args.reopen)


def session(args, out: Path, home: Path, env: dict, seconds: int) -> None:
    frames = out / 'frames'
    frames.mkdir(parents=True)
    socket = home / 'ctl.sock'
    socket.unlink(missing_ok=True)
    log = open(out / 'game.log', 'w')
    game = subprocess.Popen(
        [str(args.game.resolve()), '--resources', str(args.resources.resolve()), '--offscreen',
         '--world', 'seam', '--seed', '1', '--creative', '--render-distance', '12',
         '--width', '1280', '--height', '720', '--time', '6000', '--weather', 'clear',
         '--control-socket', str(socket)],
        cwd=home, env=env, stdout=log, stderr=subprocess.STDOUT)

    def ctl(*words, timeout=600, check=True):
        result = subprocess.run([str(args.ctl.resolve()), '--socket', str(socket), *map(str, words)],
                                cwd=home, env=env, capture_output=True, text=True, timeout=timeout)
        lines = result.stdout.strip().splitlines()
        reply = json.loads(lines[-1]) if lines else {'error': result.stderr.strip()}
        failed = result.returncode or (isinstance(reply, dict) and 'error' in reply)
        if failed and check:
            raise SystemExit(f'horizon-ctl {" ".join(map(str, words))}: {reply}')
        if failed:
            print(f'(skipped) horizon-ctl {" ".join(map(str, words))}: {reply}')
        return reply

    timeline = []
    try:
        deadline = time.time() + 300
        while not socket.exists():
            if game.poll() is not None or time.time() > deadline:
                sys.exit(f'the game did not start: see {out / "game.log"}')
            time.sleep(1)
        ctl('wait-ready', 300, timeout=330)
        ctl('command', 'gamemode spectator')
        # Noon and clear skies all along: the rules' names differ between versions.
        for rule in ('doDaylightCycle', 'advance_time', 'doWeatherCycle', 'advance_weather'):
            ctl('command', f'gamerule {rule} false', check=False)
        (x, z), y = args.at, args.height
        if args.biome:
            ctl('command', f'locate biome {args.biome}', timeout=900)
            found = re.findall(r'\[(-?\d+), (-?\d+), (-?\d+)\]', json.dumps(ctl('messages', 5)))
            if not found:
                sys.exit(f'no {args.biome} found')
            x, found_y, z = map(int, found[-1])
            y = max(args.height, found_y + 60)
            print(f'{args.biome} at {x} {found_y} {z}: the camera at y={y}', flush=True)
        ctl('command', f'tp @s {x}.5 {y} {z}.5')
        ctl('look', 0, args.pitch)
        ctl('key', 'f1', 'tap')
        start = time.time()
        index = 0
        while index * STEP <= seconds:
            # Each frame at its time from the start, whatever the screenshots take.
            wait = start + index * STEP - time.time()
            if wait > 0:
                time.sleep(wait)
            ctl('screenshot', frames / f'{index:05d}.png')
            status = ctl('status')
            lod = status.get('lod', {})
            timeline.append({'seconds': round(time.time() - start, 1), **{
                k: lod.get(k) for k in ('target_tiles', 'sampled_target_tiles', 'active_tiles',
                                        'refined_samples', 'resident_quads',
                                        'detail_target_columns', 'refined_target_columns')},
                'fps': status.get('fps'), 'frames': status.get('frames'),
                'streaming': status.get('streaming')})
            if index % 5 == 0:
                timeline[-1]['threads'] = thread_cpu(game.pid)
            print(f'{timeline[-1]}', flush=True)
            index += 1
        for yaw in DIRECTIONS:
            ctl('look', yaw, args.pitch)
            time.sleep(3)
            ctl('screenshot', out / f'yaw{yaw}.png')
        settled = settled_at(timeline)
        detailed = settled_at(timeline, 'detail_target_columns', 'refined_target_columns')
        print(f'settled ({SETTLED:.0%} of target tiles sampled) at {settled} s; '
              f'{SETTLED:.0%} of the detail at {detailed} s', flush=True)
        (out / 'status.json').write_text(json.dumps(
            {'settled_seconds': settled, 'detailed_seconds': detailed, 'timeline': timeline, 'threads': thread_cpu(game.pid),
             'final': ctl('status')}, indent=2))
        ctl('quit', check=False)
    finally:
        try:
            game.wait(timeout=60)
        except subprocess.TimeoutExpired:
            game.kill()
        log.close()


def compose(args) -> None:
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    sides = [path.resolve() for path in args.recordings]
    if len(args.labels) != len(sides):
        sys.exit('one label per recording')
    font = '/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf'
    width = 1920 // len(sides) // 2 * 2

    def label(text: str) -> str:
        text = text.replace(':', r'\:')
        return (f"scale={width}:-2,drawtext=fontfile={font}:text='{text}':x=12:y=12:fontsize=26:"
                f"fontcolor=white:box=1:boxcolor=black@0.6:boxborderw=8")

    def row(inputs: list, target: Path, extra: list) -> None:
        graph = ''.join(f'[{i}:v]{label(text)}[s{i}];' for i, text in enumerate(args.labels))
        graph += ''.join(f'[s{i}]' for i in range(len(sides)))
        graph += f'hstack=inputs={len(sides)}:shortest=1'
        subprocess.run(['ffmpeg', '-y', '-loglevel', 'error', *(a for i in inputs for a in i),
                        '-filter_complex', graph, *extra, target], check=True)

    count = min(len(list((side / 'frames').glob('*.png'))) for side in sides)
    # The loading at five times its speed (a frame every two seconds, shown at 2.5 a second).
    row([['-framerate', '2.5', '-i', side / 'frames' / '%05d.png'] for side in sides],
        out / 'loading.mp4', ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-r', '30'])
    for seconds in STILLS:
        frame = f'{seconds // STEP:05d}.png'
        if all((side / 'frames' / frame).exists() for side in sides):
            row([['-i', side / 'frames' / frame] for side in sides],
                out / f'at-{seconds:03d}s.png', ['-frames:v', '1'])
    for yaw in DIRECTIONS:
        row([['-i', side / f'yaw{yaw}.png'] for side in sides],
            out / f'settled-yaw{yaw}.png', ['-frames:v', '1'])
    for label, side in zip(args.labels, sides):
        status = json.loads((side / 'status.json').read_text())
        print(f'{label}: settled at {status.get("settled_seconds")} s, detailed at {status.get("detailed_seconds")} s; threads {status.get("threads")}')
    print(f'{out}: {count} frames a side')


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='mode', required=True)
    rec = sub.add_parser('record')
    rec.add_argument('--game', type=Path, required=True)
    rec.add_argument('--ctl', type=Path, required=True)
    rec.add_argument('--resources', type=Path, required=True)
    rec.add_argument('--out', type=Path, required=True)
    rec.add_argument('--height', type=int, default=140, help='camera height (y)')
    rec.add_argument('--pitch', type=float, default=15.0, help='degrees below the horizon')
    rec.add_argument('--seconds', type=int, default=300, help='how long to film the loading')
    rec.add_argument('--at', type=int, nargs=2, default=(0, 0), metavar=('X', 'Z'),
                     help='film above this column (seed 1: jagged peaks at -55 -1910)')
    rec.add_argument('--reopen', type=int, default=0, metavar='SECONDS',
                     help='then quit, open the same world again and film this long (in reopen/)')
    rec.add_argument('--biome', help='film above the nearest such biome, e.g. minecraft:jagged_peaks')
    comp = sub.add_parser('compose')
    comp.add_argument('recordings', type=Path, nargs='+')
    comp.add_argument('--labels', nargs='+', required=True)
    comp.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    record(args) if args.mode == 'record' else compose(args)


if __name__ == '__main__':
    main()
