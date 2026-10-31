#!/usr/bin/env python3
"""Record a video of far terrain drawn from a server's BlueMap web map, without joining the server:
a new local world (render distance 2) whose far terrain comes from the map at HORIZON_DEMO_BLUEMAP
(game.rs `prepare_lod`), filmed offscreen from high above spawn with horizon-ctl.

The video: the far terrain filling in after the world opens (a frame every two seconds), then a
full turn on the spot once it has (a frame a degree), at 30 frames a second.

    HORIZON_DEMO_BLUEMAP=https://map.example.net python3 tools/demo/bluemap_demo.py \\
        --game target/fast/horizon-client --ctl target/fast/horizon-ctl \\
        --resources .local/horizon-client/26.3 --out demo
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--game', type=Path, required=True)
    parser.add_argument('--ctl', type=Path, required=True)
    parser.add_argument('--resources', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--height', type=int, default=220, help='camera height (y)')
    parser.add_argument('--pitch', type=float, default=18.0, help='degrees below the horizon')
    parser.add_argument('--settle', type=int, default=600, help='seconds far terrain may take')
    args = parser.parse_args()
    if not os.environ.get('HORIZON_DEMO_BLUEMAP'):
        sys.exit('HORIZON_DEMO_BLUEMAP names the map')
    out = args.out.resolve()
    frames = out / 'frames'
    shutil.rmtree(out, ignore_errors=True)
    frames.mkdir(parents=True)
    # Fresh folders for everything the game writes: no player data is touched.
    home = Path(tempfile.mkdtemp(prefix='horizon-demo-'))
    env = dict(os.environ, HORIZON_HOME=str(home), HORIZON_NO_AUDIO='1', HORIZON_TELEMETRY_URL='off',
               HORIZON_LAUNCHER_NO_SELF_UPDATE='1',
               XDG_DATA_HOME=tempfile.mkdtemp(), XDG_CONFIG_HOME=tempfile.mkdtemp())
    (home / 'horizon-graphics.txt').write_text('lodDistance:128\nblueMapRadius:128\n')
    socket = home / 'ctl.sock'
    log = open(out / 'game.log', 'w')
    game = subprocess.Popen(
        [str(args.game.resolve()), '--resources', str(args.resources.resolve()), '--offscreen',
         '--world', 'demo', '--seed', '1', '--creative', '--render-distance', '2',
         '--width', '1280', '--height', '720', '--time', '6000', '--weather', 'clear',
         '--control-socket', str(socket)],
        cwd=home, env=env, stdout=log, stderr=subprocess.STDOUT)

    def ctl(*words, timeout=600):
        result = subprocess.run([str(args.ctl.resolve()), '--socket', str(socket), *map(str, words)],
                                cwd=home, env=env, capture_output=True, text=True, timeout=timeout)
        lines = result.stdout.strip().splitlines()
        reply = json.loads(lines[-1]) if lines else {'error': result.stderr.strip()}
        if result.returncode or (isinstance(reply, dict) and 'error' in reply):
            raise SystemExit(f'horizon-ctl {" ".join(map(str, words))}: {reply}')
        return reply

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
            try:
                ctl('command', f'gamerule {rule} false')
            except SystemExit as e:
                print(f'(skipped) {e}')
        ctl('command', f'tp @s 0.5 {args.height} 0.5')
        ctl('look', 0, args.pitch)
        count = 0

        def shot():
            nonlocal count
            ctl('screenshot', frames / f'{count:05d}.png')
            count += 1

        # Far terrain fills in: a frame every two seconds until every tile it wants is in and
        # stays so for 30 seconds, or `settle` passes.
        start = time.time()
        steady = None
        while time.time() - start < args.settle:
            shot()
            lod = ctl('status').get('lod', {})
            wanted, sampled = lod.get('target_tiles', 0), lod.get('sampled_target_tiles', 0)
            print(f'{time.time() - start:5.0f}s far terrain {sampled}/{wanted} tiles, '
                  f'{lod.get("active_tiles", 0)} drawn', flush=True)
            done = wanted > 0 and sampled >= wanted
            steady = (steady or time.time()) if done else None
            if steady and time.time() - steady > 30:
                break
            time.sleep(2)
        filling = count
        # A full turn on the spot.
        for degree in range(360):
            ctl('look', degree, args.pitch)
            shot()
        status = ctl('status')
        (out / 'status.json').write_text(json.dumps(status, indent=2))
        ctl('quit')
    finally:
        try:
            game.wait(timeout=60)
        except subprocess.TimeoutExpired:
            game.kill()
        log.close()
        # The map's address stays out of what is uploaded.
        url = os.environ['HORIZON_DEMO_BLUEMAP']
        host = url.split('://', 1)[-1].split('/', 1)[0]
        text = (out / 'game.log').read_text(errors='replace').replace(url, '<map>').replace(host, '<map>')
        (out / 'game.log').write_text(text)
    # Filling in sped up (a frame every two seconds, shown at 10 a second), then the turn at 30.
    ffmpeg = ['ffmpeg', '-y', '-loglevel', 'error']
    subprocess.run(ffmpeg + ['-framerate', '10', '-i', frames / '%05d.png', '-frames:v', str(filling),
                             '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-r', '30', out / 'filling.mp4'], check=True)
    subprocess.run(ffmpeg + ['-framerate', '30', '-start_number', str(filling), '-i', frames / '%05d.png',
                             '-c:v', 'libx264', '-pix_fmt', 'yuv420p', out / 'turn.mp4'], check=True)
    (out / 'parts.txt').write_text("file 'filling.mp4'\nfile 'turn.mp4'\n")
    subprocess.run(ffmpeg + ['-f', 'concat', '-safe', '0', '-i', out / 'parts.txt', '-c', 'copy',
                             out / 'bluemap-far-terrain.mp4'], check=True)
    print(f'{out / "bluemap-far-terrain.mp4"}: {filling} frames filling in, 360 turning')


if __name__ == '__main__':
    main()
