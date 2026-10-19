"""Local, isolated real-network acceptance; never contacts a public endpoint."""
import base64
import ipaddress
import json
import os
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

binary, scheme, *reference = sys.argv[1:]
assert len(reference) <= 1, 'at most one reference checker'
errors = []
proxy_targets = []
dns_queries = []
services = []
failed = threading.Event()


def exact(stream, count):
    result = b''
    while len(result) < count:
        part = stream.recv(count - len(result))
        if not part:
            raise EOFError()
        result += part
    return result


def frame(stream):
    return exact(stream, struct.unpack('!H', exact(stream, 2))[0])


def sendframe(stream, data):
    stream.sendall(struct.pack('!H', len(data)) + data)


def spawn(function, *arguments):
    def run():
        try:
            function(*arguments)
        except (EOFError, ConnectionError):
            pass
        except BaseException as error:
            errors.append(repr(error))
    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def answer(query, internal=False):
    end = 12
    while query[end]:
        end += query[end] + 1
    end += 5
    count = 4093 if query[12:17] == b'\x04huge' else 80 if query[12:18] == b'\x05large' else 1
    opt = query[end:] if query[10:12] == b'\x00\x01' else b''
    # Omit OPT for the near-65535 response, so it is a legal TCP DNS frame.
    if count == 4093:
        opt = b''
    result = query[:2] + struct.pack('!HHHHH', 0x8180, 1, count, 0, bool(opt)) + query[12:end]
    for index in range(count):
        ip = f'198.51.100.{index % 254 + 1 if count > 1 else 124 if internal else 123}'
        result += b'\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x3c\x00\x04' + socket.inet_aton(ip)
    return result + opt


def tcpserve(listener):
    while True:
        stream, _ = listener.accept()

        def echo(stream):
            with stream:
                while data := stream.recv(65536):
                    stream.sendall(data)
        spawn(echo, stream)


def udpserve(stream):
    while True:
        data, source = stream.recvfrom(65536)
        stream.sendto(data, source)


ports = {}
for family, ip, suffix in [(socket.AF_INET, '127.0.0.1', '4'), (socket.AF_INET6, '::1', '6'),
                           (socket.AF_INET, '192.168.65.1', 'lan'), (socket.AF_INET6, 'fdc5:1234::1', 'lan6')]:
    for kind, protocol in [('tcp', socket.SOCK_STREAM), ('udp', socket.SOCK_DGRAM)]:
        stream = socket.socket(family, protocol)
        stream.bind((ip, 0))
        ports[kind + suffix] = stream.getsockname()[1]
        if kind == 'tcp':
            stream.listen()
        spawn(tcpserve if kind == 'tcp' else udpserve, stream)
        services.append(stream)

internal_udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
internal_udp.bind(('127.0.0.1', 0))
internal_port = internal_udp.getsockname()[1]


def local_dns_udp():
    while True:
        data, source = internal_udp.recvfrom(65536)
        internal_udp.sendto(answer(data, True), source)


internal_tcp = socket.socket()
internal_tcp.bind(('127.0.0.1', internal_port))
internal_tcp.listen()


def local_dns_tcp():
    while True:
        stream, _ = internal_tcp.accept()

        def connection(stream):
            with stream:
                data = frame(stream)
                sendframe(stream, answer(data, True))
        spawn(connection, stream)


spawn(local_dns_udp)
spawn(local_dns_tcp)
proxy = socket.socket()
proxy.bind(('127.0.0.1', 0))
proxy.listen()

with tempfile.TemporaryDirectory(prefix='envwrap-network-test-') as root:
    host_identity = {path: pathlib.Path(path).read_bytes()
                     for path in ['/etc/machine-id', '/etc/localtime', '/proc/sys/kernel/hostname']}
    root = pathlib.Path(root)
    home = root / 'home'
    config = root / 'config'
    workspace = root / 'workspace'
    claude = root / 'claude-config'
    for directory in [home, config, workspace, claude]:
        directory.mkdir()
    (claude / 'settings.json').write_text('{"user-setting":true}')
    unix = socket.socket(socket.AF_UNIX)
    unix.bind(str(root / 'agent.sock'))
    unix.listen()
    spawn(tcpserve, unix)
    stopped = root / 'stopped'
    callback = root / 'callback.json'
    env = dict(PATH='/usr/bin:/usr/sbin:/bin', HOME=str(home), XDG_CONFIG_HOME=str(config),
               CLAUDE_CONFIG_DIR=str(claude), CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='user-choice',
               TZ='Pacific/Kiritimati', TZDIR='/nonexistent-envwrap-zoneinfo', LANG='C', LC_ALL='C',
               TEST_PROXY=f'{scheme}://private-user:private-password@127.0.0.1:{proxy.getsockname()[1]}',
               HTTP_PROXY='http://must-not-inherit:secret@127.0.0.1:9', HTTPS_PROXY='invalid-unused',
               KEEP_APPLICATION_SETTING='preserved', SSH_AUTH_SOCK=str(root / 'agent.sock'),
               TEST_PORTS=json.dumps(ports), TEST_STOPPED=str(stopped), TEST_CALLBACK=str(callback),
               TEST_HOME=str(home), TEST_CWD=str(workspace),
               TEST_PROFILE=str(config / 'envwrap/profiles/default.toml'))

    def proxyserve():
        while True:
            stream, _ = proxy.accept()

            def connection(stream):
                with stream:
                    stream.settimeout(5)
                    if scheme == 'http':
                        request = b''
                        while not request.endswith(b'\r\n\r\n'):
                            request += exact(stream, 1)
                        assert b'Proxy-Authorization: Basic ' + base64.b64encode(b'private-user:private-password') in request
                        target = request.split(b' ')[1].decode()
                        ip, port = target.rsplit(':', 1)
                        ip, port = ip.strip('[]'), int(port)
                        prefix = b'HTTP/1.1 200 OK\r\n\r\n'
                    else:
                        assert exact(stream, 3) == b'\x05\x01\x02'
                        stream.sendall(b'\x05\x02')
                        version, size = exact(stream, 2)
                        assert version == 1 and exact(stream, size) == b'private-user'
                        size = exact(stream, 1)[0]
                        assert exact(stream, size) == b'private-password'
                        stream.sendall(b'\x01\x00')
                        version, command, reserved, kind = exact(stream, 4)
                        assert (version, command, reserved) == (5, 1, 0) and kind in [1, 4]
                        ip = socket.inet_ntop(socket.AF_INET if kind == 1 else socket.AF_INET6, exact(stream, 4 if kind == 1 else 16))
                        port = struct.unpack('!H', exact(stream, 2))[0]
                        prefix = b'\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x01'
                    proxy_targets.append((ip, port))
                    assert ip in ['203.0.113.9', '2001:db8::9', '2001:db8::53'], (ip, port)
                    if failed.is_set() or port == 446:
                        if port == 446:
                            time.sleep(.1)
                        stream.sendall(b'HTTP/1.1 503 Unavailable\r\n\r\n' if scheme == 'http' else b'\x05\x05\x00\x01\x00\x00\x00\x00\x00\x00')
                        return
                    stream.settimeout(None)
                    stream.sendall(prefix)
                    if port == 53:
                        data = frame(stream)
                        dns_queries.append(data)
                        sendframe(stream, answer(data))
                    elif port == 444:
                        stream.sendall(b'X')
                        while stream.recv(1):
                            pass
                        failed.set()
                        stopped.touch()
                    else:
                        stream.sendall(b'READY')
                        while data := stream.recv(65536):
                            stream.sendall(data)
            spawn(connection, stream)

    spawn(proxyserve)

    def host_callbacks():
        deadline = time.monotonic() + 30
        while not callback.exists():
            assert time.monotonic() < deadline
            time.sleep(.02)
        for address in json.loads(callback.read_text()):
            deadline = time.monotonic() + 3
            while True:
                try:
                    stream = socket.create_connection(address, timeout=2)
                    break
                except ConnectionRefusedError:
                    assert time.monotonic() < deadline
                    time.sleep(.02)
            with stream:
                stream.sendall(b'host-callback')
                assert exact(stream, 13) == b'host-callback'


    def run(*arguments, check=True):
        result = subprocess.run([binary, *arguments], env=env, cwd=workspace, capture_output=True, timeout=45)
        if check:
            assert result.returncode == 0, (result.returncode, result.stdout.decode(), result.stderr.decode(), errors)
        return result

    run('profile', 'create', 'default', '--proxy-env', 'TEST_PROXY', '--dns', '[2001:db8::53]:53',
        '--timezone', 'Europe/Berlin', '--locale', 'C.UTF-8', '--direct', '192.168.65.1/32',
        '--direct', 'fdc5:1234::1/128', '--internal-dns', f'internal.test=127.0.0.1:{internal_port}')
    import environment
    observation = environment.run(binary, env, workspace, reference[0] if reference else None)
    print('ENVWRAP_ENVIRONMENT_RESULT=' + json.dumps(observation), flush=True)
    # Real filesystem effects cover option ownership, reserved names and non-UTF8 executable paths.
    run('/usr/bin/touch', '--', '--profile')
    assert (workspace / '--profile').is_file()
    (workspace / 'doctor').symlink_to('/usr/bin/python3')
    old_path = env['PATH']
    env['PATH'] = str(workspace) + ':' + old_path
    try:
        run('--', 'doctor', '-c', "from pathlib import Path; Path('reserved-command-marker').touch()")
    finally:
        env['PATH'] = old_path
    assert (workspace / 'reserved-command-marker').is_file()
    non_utf8 = os.fsencode(workspace) + b'/tool-\xff'
    os.symlink('/usr/bin/python3', non_utf8)
    run(non_utf8, '-c', "from pathlib import Path; Path('non-utf8-command-marker').touch()")
    assert (workspace / 'non-utf8-command-marker').is_file()
    print('generic command: native option ownership, reserved-name escape and non-UTF8 executable PASS')
    thread = spawn(host_callbacks)
    result = run('/usr/bin/python3', str(pathlib.Path(__file__).with_name('network_client.py')))
    print(result.stdout.decode(), end='')
    assert (workspace / 'application-write').read_text() == 'original filesystem'
    assert (claude / 'settings.json').read_text() == '{"user-setting":true}'
    thread.join(3)
    assert not thread.is_alive()
    assert not errors, errors
    assert all(b'\x08internal\x04test\x00' not in query.lower() for query in dns_queries)
    assert all(not ipaddress.ip_address(ip).is_loopback for ip, _ in proxy_targets)
    marker = workspace / 'should-not-run'
    result = run('/usr/bin/touch', str(marker), check=False)
    assert result.returncode != 0 and not marker.exists()
    failed.clear()
    result = run('doctor')
    print(result.stdout.decode(), end='')
    result = run('/bin/sh', '-c', 'exit 37', check=False)
    assert result.returncode == 37
    print(f'{scheme}: actual final CLI, authenticated proxy, identity/config preservation and fail-closed startup PASS')
    snapshot = "import json,pathlib,socket; print(json.dumps([socket.gethostname()]+[pathlib.Path(p).read_text() for p in ['/etc/machine-id','/proc/sys/kernel/random/boot_id','/sys/class/net/eth0/address']]))"
    before = pathlib.Path(env['TEST_PROFILE']).read_bytes()
    first = run('/usr/bin/python3', '-c', snapshot).stdout
    second = run('/usr/bin/python3', '-c', snapshot).stdout
    assert json.loads(first) == json.loads(second)
    assert pathlib.Path(env['TEST_PROFILE']).read_bytes() == before
    assert all(pathlib.Path(path).read_bytes() == contents for path, contents in host_identity.items())
    print('stable profile identity/boot view across launches; host files unchanged: PASS')
    import lifecycle
    lifecycle.run(binary, env, workspace)
    import development
    development.run(binary, env, workspace)
