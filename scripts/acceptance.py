#!/usr/bin/env python3
"""Independent environment/network acceptance. No Claude login, API calls or real profiles."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
STANDARD = 'envwrap-independent-v1'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def run_checks(binary, reference_checker=None):
    binary = Path(binary).resolve(strict=True)
    checker = Path(reference_checker).resolve(strict=True) if reference_checker else None
    version = subprocess.check_output([str(binary), '--version'], text=True, timeout=10).strip()
    if not version.startswith('envwrap '):
        raise ValueError('acceptance requires the actual envwrap executable')
    report = {
        'schema_version': 1, 'standard': STANDARD, 'binary_version': version,
        'binary_sha256': digest(binary), 'scenarios': {},
        'reference_checker': {'status': 'NOT_EXECUTED', 'reason': 'optional external checker not supplied'},
        'application_internals': {'status': 'OUT_OF_SCOPE', 'reason': 'closed-source telemetry payloads and service-side decisions are not an environment acceptance criterion'},
        'application_sessions': {'status': 'NOT_EXECUTED', 'reason': 'optional compatibility evidence, not a release gate'},
        'declared_exceptions': ['host loopback', 'explicit LAN/internal DNS', 'filesystem Unix sockets',
                                'real kernel/CPU/memory/storage', '50 ms TCP callback discovery interval'],
    }
    if checker:
        report['reference_checker'] = {
            'status': 'NOT_EXECUTED', 'sha256': digest(checker),
            'version': subprocess.check_output([str(checker), '--version'], text=True, timeout=10).strip(),
        }
    for scheme in ('http', 'socks5h'):
        command = [
            'bwrap', '--bind', '/', '/', '--dev-bind', '/dev', '/dev', '--proc', '/proc',
            '--unshare-user', '--unshare-net', '--cap-add', 'CAP_NET_ADMIN', '--cap-add', 'CAP_NET_RAW',
            '--die-with-parent', '--', '/bin/sh', str(ROOT / 'tests/fixtures/network_namespace.sh'),
            str(ROOT / 'tests/fixtures/network.py'), str(binary), scheme,
        ]
        if checker:
            command.append(str(checker))
        try:
            result = subprocess.run(command, cwd=ROOT, env={'PATH': '/usr/bin:/usr/sbin:/bin'},
                                    capture_output=True, timeout=180)
            # Fixture logs contain verdicts and synthetic test configuration, not raw checker reports.
            sys.stderr.write(result.stdout.decode(errors='replace'))
            sys.stderr.write(result.stderr.decode(errors='replace'))
            observations = [json.loads(line.split(b'=', 1)[1]) for line in result.stdout.splitlines()
                            if line.startswith(b'ENVWRAP_ENVIRONMENT_RESULT=')]
            complete = len(observations) == 1 and observations[0].get('status') == 'PASS'
            report['scenarios'][scheme] = {
                'status': 'PASS' if result.returncode == 0 and complete else 'FAIL',
                'exit_code': result.returncode,
                'environment': observations[0] if complete else {'status': 'NOT_EXECUTED'},
            }
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            report['scenarios'][scheme] = {'status': 'NOT_EXECUTED', 'reason': type(error).__name__}
    report['passed'] = all(item['status'] == 'PASS' for item in report['scenarios'].values())
    if checker:
        report['reference_checker']['status'] = 'PASS' if report['passed'] else 'INCOMPLETE'
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/envwrap')
    parser.add_argument('--reference-checker', type=Path, help='optional read-only claude-code-env-checker cross-check')
    parser.add_argument('--report', type=Path, help='write a new private JSON report; existing files are not overwritten')
    args = parser.parse_args()
    try:
        report = run_checks(args.binary, args.reference_checker)
        rendered = json.dumps(report, indent=2) + '\n'
        if args.report:
            fd = os.open(args.report, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as stream:
                stream.write(rendered)
        sys.stdout.write(rendered)
        return 0 if report['passed'] else 1
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        # Do not accidentally echo subprocess output or values from an observation.
        print('acceptance could not complete: ' + type(error).__name__, file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
