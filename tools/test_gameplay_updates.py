#!/usr/bin/env python3
"""Regression tests for dedicated fluid/block ticks and multiplayer item drops.

Uses the real game, prepared resources and packet handlers without a display,
network service, account or changes to an existing world.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/horizon-client')
    parser.add_argument('--resources', type=Path, default=ROOT / '.local/horizon-client/26.3')
    parser.add_argument('--output', type=Path, default=ROOT / '.local/gameplay-updates')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    report = (args.output / 'report.json').resolve()
    report.unlink(missing_ok=True)
    env = dict(os.environ, HORIZON_GAMEPLAY_FIXTURE=str(report))
    with tempfile.TemporaryDirectory(prefix='horizon-gameplay-') as work:
        # Dedicated mode silences audio; the fixture exits before starting a server.
        Path(work, 'eula.txt').write_text('eula=true\n')
        with (args.output / 'run.log').open('w') as log:
            result = subprocess.run(
                [str(args.binary.resolve()), '--resources', str(args.resources.resolve()), '--dedicated', '--nogui'],
                cwd=work, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=180,
            )
    if not report.exists():
        raise SystemExit(f'Fixture did not write a report; see {args.output / "run.log"}')
    data = json.loads(report.read_text())
    for check in data['checks']:
        print(f'{"PASS" if check["passed"] else "FAIL"} {check["name"]}')
    if result.returncode or not data['passed']:
        raise SystemExit(f'Gameplay regression failed: {report}')
    print(f'{len(data["checks"])} gameplay checks passed: {report}')


if __name__ == '__main__':
    main()
