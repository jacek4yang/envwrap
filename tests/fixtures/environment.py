"""Independent syscall/filesystem observations; no envwrap implementation imports.

Probe values are fingerprinted before leaving the observed process. Final output
contains verdicts only. Optional reference-checker JSON stays in memory, never on disk.
"""
import hashlib
import json
import os
import pathlib
import pwd
import socket
import subprocess
import sys
import time
import tomllib


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def read(path):
    return pathlib.Path(path).read_text().strip()


def collect():
    observations = {}

    def probe(key, operation):
        try:
            observations[key] = {'status': 'ok', 'fingerprint': fingerprint(operation())}
        except FileNotFoundError:
            observations[key] = {'status': 'missing'}
        except PermissionError:
            observations[key] = {'status': 'permission_denied'}
        except OSError:
            observations[key] = {'status': 'error'}

    probe('hostname.syscall', socket.gethostname)
    probe('hostname.uname', lambda: os.uname().nodename)
    for key, path in {
        'hostname.etc': '/etc/hostname', 'hostname.proc': '/proc/sys/kernel/hostname',
        'machine.etc': '/etc/machine-id', 'machine.dbus': '/var/lib/dbus/machine-id',
        'boot': '/proc/sys/kernel/random/boot_id', 'timezone.name': '/etc/timezone',
        'os.release': '/etc/os-release',
    }.items():
        probe(key, lambda path=path: read(path))
    probe('machine.dbus_canonical', lambda: read(os.path.realpath('/var/lib/dbus/machine-id')))
    probe('kernel', lambda: [os.uname().sysname, os.uname().release, os.uname().version, os.uname().machine])
    probe('cpu.models_flags', lambda: [line.strip() for line in read('/proc/cpuinfo').splitlines()
                                      if line.split(':', 1)[0].strip() in ('processor', 'vendor_id', 'model name', 'flags')])
    probe('cpu.affinity', lambda: sorted(os.sched_getaffinity(0)))
    probe('memory.total', lambda: next(line for line in read('/proc/meminfo').splitlines() if line.startswith('MemTotal:')))
    probe('storage.devices', lambda: sorted(os.listdir('/sys/block')))
    probe('storage.capacity', lambda: [os.statvfs('/').f_blocks, os.statvfs('/').f_frsize])
    probe('user.ids', lambda: [os.getuid(), os.geteuid(), os.getgid(), os.getegid()])
    probe('user.passwd', lambda: list(pwd.getpwuid(os.getuid())))
    probe('user.environment', lambda: {key: os.environ.get(key) for key in (
        'HOME', 'PATH', 'CLAUDE_CONFIG_DIR', 'SSH_AUTH_SOCK', 'KEEP_APPLICATION_SETTING',
        'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC')})
    probe('user.cwd', os.getcwd)
    probe('timezone.symlink', lambda: os.readlink('/etc/localtime'))
    probe('timezone.bytes', lambda: pathlib.Path('/etc/localtime').read_bytes().hex())
    probe('timezone.offsets', lambda: [time.localtime(epoch).tm_gmtoff for epoch in (1700000000, 1720000000)])
    probe('identity.environment', lambda: {key: os.environ.get(key) for key in ('HOSTNAME', 'TZ', 'TZDIR', 'LANG', 'LC_ALL')})
    probe('proxy.variables', lambda: sorted(key for key in os.environ if key == 'TEST_PROXY'
                                          or key.lower() in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy', 'ftp_proxy')))
    for kind in ('mnt', 'net', 'pid', 'uts', 'user'):
        probe('namespace.' + kind, lambda kind=kind: os.readlink('/proc/self/ns/' + kind))
    for key in ('CapInh', 'CapPrm', 'CapEff', 'CapAmb', 'NoNewPrivs'):
        probe('privilege.' + key, lambda key=key: next(line.split(':', 1)[1].strip()
                                                     for line in read('/proc/self/status').splitlines() if line.startswith(key + ':')))
    probe('network.sysclass', lambda: sorted(os.listdir('/sys/class/net')))
    probe('network.sysdevices', lambda: sorted(os.listdir('/sys/devices/virtual/net')))
    probe('network.if_nameindex', lambda: sorted(name for _, name in socket.if_nameindex()))
    probe('network.proc', lambda: sorted(line.split(':', 1)[0].strip() for line in read('/proc/net/dev').splitlines() if ':' in line))
    for alias, path in (('class', '/sys/class/dmi/id'), ('devices', '/sys/devices/virtual/dmi/id')):
        for field in ('sys_vendor', 'product_name', 'product_uuid', 'product_serial', 'board_serial', 'chassis_serial'):
            probe('dmi.' + alias + '.' + field, lambda path=path, field=field: read(path + '/' + field))
    return observations


def require(condition, field):
    if not condition:
        raise AssertionError('independent environment acceptance failed: ' + field)


def value(report, key):
    require(report.get(key, {}).get('status') == 'ok', key + ' unavailable (not a pass)')
    return report[key]['fingerprint']


def expect(report, key, expected):
    require(value(report, key) == fingerprint(expected), key + ' inconsistent')


def reference_check(checker, binary, env, cwd, identity, observed):
    def scan(prefix):
        result = subprocess.run([*prefix, checker, 'scan', '--compact', '--include-sensitive', '--no-commands'],
                                env=env, cwd=cwd, capture_output=True, timeout=30)
        require(result.returncode == 0, 'reference checker could not run')
        envelope = json.loads(result.stdout)
        require(envelope.get('ok') is True and envelope.get('schema_version') == '1.0', 'reference checker schema/status')
        return envelope['data']['sections']

    host, child = scan([]), scan([binary, '--'])

    def get(report, section, field):
        probe = report[section][field]
        require(probe['status'] == 'ok', 'reference.' + section + '.' + field + ' unavailable (not a pass)')
        return probe['value']

    require(get(child, 'os', 'hostname') == identity['hostname'], 'reference.hostname')
    for field, expected in [('machine_id', identity['machine_id']), ('dbus_machine_id', identity['machine_id']),
                            ('boot_id_characteristics', None), ('dmi_product_uuid', identity['product_uuid']),
                            ('dmi_product_serial', identity['machine_id'])]:
        independent = {'dbus_machine_id': 'machine.dbus', 'dmi_product_uuid': 'dmi.class.product_uuid',
                       'dmi_product_serial': 'dmi.class.product_serial'}.get(field)
        if independent and observed[independent]['status'] == 'missing':
            require(child['machine'][field]['status'] == 'not_found', 'reference.' + field + ' absence')
            continue
        result = get(child, 'machine', field)['value']
        if expected is None:
            require(fingerprint(result) == value(observed, 'boot'), 'reference.boot')
        else:
            require(result == expected, 'reference.' + field)
    for field in ('effective_timezone', 'system_timezone'):
        require(get(child, 'time_locale', field) == identity['timezone'], 'reference.' + field)
    require(get(child, 'time_locale', 'conflicts') == [], 'reference.timezone.conflicts')
    for field in ('lang', 'lc_all'):
        require(get(child, 'time_locale', field) == identity['locale'], 'reference.' + field)
    interfaces = get(child, 'network_host', 'interfaces')
    require(sorted(item['name'] for item in interfaces) == ['eth0', 'lo'], 'reference.network.interfaces')
    require(next(item['mac_address'] for item in interfaces if item['name'] == 'eth0') == identity['mac'], 'reference.network.mac')
    require(get(child, 'network_host', 'resolv_conf')['nameservers'] == ['198.18.0.1'], 'reference.resolver')
    for section, fields in {'os': ('kernel_release', 'architecture', 'uid', 'euid', 'gid', 'egid'),
                            'cpu': ('model', 'flags', 'logical_cores', 'memory_total_bytes')}.items():
        for field in fields:
            require(get(host, section, field) == get(child, section, field), 'reference.inherited.' + field)
    print('independent reference checker: identity/timezone/interfaces/resolver/inherited hardware PASS; risk score is advisory', flush=True)


def run(binary, env, cwd, checker=None):
    def observe(prefix):
        result = subprocess.run([*prefix, '/usr/bin/python3', '-B', str(pathlib.Path(__file__).resolve()), '--probe'],
                                env=env, cwd=cwd, capture_output=True, timeout=30)
        require(result.returncode == 0, 'observation process did not complete')
        return json.loads(result.stdout)

    profile = tomllib.loads(pathlib.Path(env['TEST_PROFILE']).read_text())
    identity = profile['identity']
    host, child = observe([]), observe([binary, '--'])
    for key in ('kernel', 'os.release', 'cpu.models_flags', 'cpu.affinity', 'memory.total',
                'storage.devices', 'storage.capacity', 'user.ids', 'user.passwd', 'user.environment', 'user.cwd'):
        require(value(host, key) == value(child, key), key + ' intentionally inherited')
    for key in ('hostname.syscall', 'hostname.uname', 'hostname.etc', 'hostname.proc'):
        expect(child, key, identity['hostname'])
    expect(child, 'machine.etc', identity['machine_id'])
    for key in ('machine.dbus', 'machine.dbus_canonical'):
        if host[key]['status'] == 'missing':
            require(child[key]['status'] == 'missing', key + ' unsupported alias must not be created')
            print(key + ': UNSUPPORTED (absent in baseline)', flush=True)
        else:
            expect(child, key, identity['machine_id'])
    expect(child, 'timezone.name', identity['timezone'])
    expect(child, 'timezone.symlink', '/usr/share/zoneinfo/' + identity['timezone'])
    expect(child, 'timezone.bytes', pathlib.Path('/usr/share/zoneinfo', identity['timezone']).read_bytes().hex())
    expect(child, 'timezone.offsets', [3600, 7200])  # Fixed winter/summer instants; fixture uses Europe/Berlin.
    expect(child, 'identity.environment', dict(HOSTNAME=identity['hostname'], TZ=identity['timezone'],
                                              TZDIR=None, LANG=identity['locale'], LC_ALL=identity['locale']))
    expect(child, 'proxy.variables', [])
    for kind in ('mnt', 'net', 'pid', 'uts', 'user'):
        require(value(host, 'namespace.' + kind) != value(child, 'namespace.' + kind), 'namespace.' + kind)
    for key in ('CapInh', 'CapPrm', 'CapEff', 'CapAmb'):
        expect(child, 'privilege.' + key, '0000000000000000')
    expect(child, 'privilege.NoNewPrivs', '1')
    for key in ('network.sysclass', 'network.sysdevices', 'network.if_nameindex', 'network.proc'):
        expect(child, key, ['eth0', 'lo'])
    for alias in ('class', 'devices'):
        if host['dmi.' + alias + '.sys_vendor']['status'] == 'missing':
            require(child['dmi.' + alias + '.sys_vendor']['status'] == 'missing', 'DMI absent baseline')
            print('dmi.' + alias + ': UNSUPPORTED (absent in baseline)', flush=True)
            continue
        for field, expected in {'sys_vendor': 'envwrap', 'product_name': 'Linux container',
                                'product_uuid': identity['product_uuid'], 'product_serial': identity['machine_id'],
                                'board_serial': identity['machine_id'], 'chassis_serial': identity['machine_id']}.items():
            expect(child, 'dmi.' + alias + '.' + field, expected)
    repeat = observe([binary, '--'])
    require(value(host, 'boot') != value(child, 'boot') == value(repeat, 'boot'), 'boot profile scope and restart stability')
    if checker:
        reference_check(checker, binary, env, cwd, identity, child)
    else:
        print('independent reference checker: NOT_EXECUTED (optional tool not supplied; built-in observations remain mandatory)', flush=True)
    after = observe([])
    for key in ('hostname.syscall', 'hostname.etc', 'machine.etc', 'boot', 'timezone.name', 'timezone.bytes'):
        require(host[key] == after[key], 'host mutation: ' + key)
    print('independent observations: syscall/procfs/sysfs aliases, namespaces, winter/summer TZ, inherited hardware/storage/config PASS', flush=True)
    return {
        'status': 'PASS',
        'declared_identity_and_aliases': 'PASS',
        'timezone_winter_summer_and_locale': 'PASS',
        'namespace_and_capability_isolation': 'PASS',
        'network_inventory_cross_views': 'PASS',
        'kernel_cpu_memory_storage_user_config': 'INTENTIONALLY_INHERITED',
        'host_unchanged': 'PASS',
        'conditional_surfaces': {key: 'UNSUPPORTED' for key, item in child.items() if item['status'] == 'missing'},
        'reference_checker': 'PASS' if checker else 'NOT_EXECUTED',
    }


if __name__ == '__main__':
    if sys.argv[1:] != ['--probe']:
        raise SystemExit('Internal fingerprint-only observation helper; use scripts/acceptance.py')
    print(json.dumps(collect()))
