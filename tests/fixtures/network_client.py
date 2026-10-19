"""Actual unprivileged application inside envwrap; no proxy-aware client code."""
import fcntl
import ipaddress
import json
import os
import pathlib
import socket
import struct
import subprocess
import threading
import time
import tomllib

ports = json.loads(os.environ['TEST_PORTS'])
profile = tomllib.loads(pathlib.Path(os.environ['TEST_PROFILE']).read_text())
identity = profile['identity']
assert 'TEST_PROXY' not in os.environ
assert 'HTTP_PROXY' not in os.environ
assert 'HTTPS_PROXY' not in os.environ
assert os.environ['KEEP_APPLICATION_SETTING'] == 'preserved'
assert os.environ['CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC'] == 'user-choice'
assert pathlib.Path.home() == pathlib.Path(os.environ['TEST_HOME'])
assert pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'], 'settings.json').read_text() == '{"user-setting":true}'
assert pathlib.Path.cwd() == pathlib.Path(os.environ['TEST_CWD'])
assert socket.gethostname() == identity['hostname']
assert pathlib.Path('/etc/hostname').read_text().strip() == identity['hostname']
assert pathlib.Path('/proc/sys/kernel/hostname').read_text().strip() == identity['hostname']
for path in ['/etc/machine-id', '/var/lib/dbus/machine-id']:
    if pathlib.Path(path).exists():
        assert pathlib.Path(path).read_text().strip() == identity['machine_id']
assert os.environ['TZ'] == pathlib.Path('/etc/timezone').read_text().strip() == identity['timezone']
assert os.readlink('/etc/localtime') == '/usr/share/zoneinfo/' + identity['timezone']
assert pathlib.Path('/etc/localtime').read_bytes() == pathlib.Path('/usr/share/zoneinfo', identity['timezone']).read_bytes()
assert time.localtime(1700000000).tm_gmtoff == 3600  # Europe/Berlin in November 2023.
without_tz = dict(os.environ)
without_tz.pop('TZ')
offset = subprocess.check_output(['/usr/bin/date', '-d', '@1700000000', '+%z'], env=without_tz).strip()
assert offset == b'+0100'  # /etc/localtime consumers agree with TZ/libc consumers.
assert os.environ['LANG'] == os.environ['LC_ALL'] == identity['locale']
assert subprocess.check_output(['locale', 'charmap']).strip() == b'UTF-8'
assert sorted(os.listdir('/sys/class/net')) == ['eth0', 'lo']
assert sorted(name for _, name in socket.if_nameindex()) == ['eth0', 'lo']
assert pathlib.Path('/sys/class/net/eth0/address').read_text().strip() == identity['mac']
addresses = json.loads(subprocess.check_output(['ip', '-j', 'address', 'show', 'dev', 'eth0']))
with socket.socket(socket.AF_INET) as interface_socket:
    address = fcntl.ioctl(interface_socket.fileno(), 0x8915, struct.pack('256s', b'eth0'))[20:24]
    assert socket.inet_ntoa(address) in {item['local'] for item in addresses[0]['addr_info']}
proc_ipv6 = {str(ipaddress.IPv6Address(bytes.fromhex(line.split()[0])))
             for line in pathlib.Path('/proc/net/if_inet6').read_text().splitlines()
             if line.split()[-1] == 'eth0'}
assert proc_ipv6 == {item['local'] for item in addresses[0]['addr_info'] if item['family'] == 'inet6'}
assert addresses[0]['address'] == identity['mac']
for alias in ['/sys/class/dmi/id', '/sys/devices/virtual/dmi/id']:
    if pathlib.Path(alias).exists():
        assert pathlib.Path(alias, 'product_uuid').read_text().strip() == identity['product_uuid']
        assert pathlib.Path(alias, 'product_serial').read_text().strip() == identity['machine_id']
        assert pathlib.Path(alias, 'sys_vendor').read_text().strip() == 'envwrap'
for line in pathlib.Path('/proc/self/status').read_text().splitlines():
    if line.startswith(('CapEff:', 'CapPrm:', 'CapInh:', 'CapAmb:')):
        assert int(line.split()[1], 16) == 0
assert 'processor' in pathlib.Path('/proc/cpuinfo').read_text()
assert 'MemTotal:' in pathlib.Path('/proc/meminfo').read_text()
pathlib.Path(os.environ['TEST_CWD'], 'application-write').write_text('original filesystem')
with socket.socket(socket.AF_UNIX) as unix:
    unix.connect(os.environ['SSH_AUTH_SOCK'])
    unix.sendall(b'preserved Unix socket')
    assert unix.recv(100) == b'preserved Unix socket'
print('identity aliases/netlink/sysfs/locale, capability drop, native config/filesystem/Unix socket: PASS', flush=True)


def exact(stream, count):
    result = b''
    while len(result) < count:
        part = stream.recv(count - len(result))
        assert part, 'short stream response'
        result += part
    return result


def tcp_roundtrip(address, banner=False):
    with socket.create_connection(address, timeout=4) as stream:
        if banner:
            assert exact(stream, 5) == b'READY'
        payload = bytes(range(256)) * 1024
        stream.sendall(payload)
        stream.shutdown(socket.SHUT_WR)
        assert exact(stream, len(payload)) == payload
        assert stream.recv(1) == b''


for address in [('203.0.113.9', 443), ('2001:db8::9', 443)]:
    tcp_roundtrip(address, True)
for family, ip in [(socket.AF_INET, '203.0.113.9'), (socket.AF_INET6, '2001:db8::9')]:
    with socket.socket(family) as stream:
        stream.settimeout(2)
        stream.connect((ip, 446))
        stream.sendall(b'already-acknowledged-data')
        try:
            assert stream.recv(1) == b''
        except ConnectionError:
            pass
print('transparent TCP v4/v6, prefetched banner, half-close, late refusal: PASS', flush=True)

with socket.create_connection(('203.0.113.9', 443), timeout=4) as stream:
    assert exact(stream, 5) == b'READY'
    stream.sendall(b'before-idle')
    assert exact(stream, 11) == b'before-idle'
    time.sleep(11)  # Beyond the transport's ten-second setup deadline.
    stream.sendall(b'after-idle')
    assert exact(stream, 10) == b'after-idle'
print('incremental streaming survives established-session idle past setup deadline: PASS', flush=True)

for family, ip, key in [(socket.AF_INET, '127.0.0.1', 'tcp4'), (socket.AF_INET6, '::1', 'tcp6')]:
    tcp_roundtrip((ip, ports[key]))
    listener = socket.socket(family)
    listener.bind((ip, 0))
    listener.listen()

    def local_server(listener=listener):
        with listener:
            stream, _ = listener.accept()
            with stream:
                while data := stream.recv(65536):
                    stream.sendall(data)

    thread = threading.Thread(target=local_server)
    thread.start()
    tcp_roundtrip(listener.getsockname()[:2])
    thread.join(3)
    assert not thread.is_alive()
for ip, key in [('192.168.65.1', 'tcplan'), ('fdc5:1234::1', 'tcplan6')]:
    tcp_roundtrip((ip, ports[key]))
print('host and child-local TCP plus explicit LAN v4/v6: PASS', flush=True)


def named(name, edns=0):
    result = struct.pack('!HHHHHH', 71, 0x100, 1, 0, 0, int(bool(edns)))
    for label in name.split('.'):
        result += bytes([len(label)]) + label.encode()
    result += b'\x00\x00\x01\x00\x01'
    if edns:
        result += b'\x00\x00\x29' + struct.pack('!H', edns) + b'\x00\x00\x00\x00\x00\x00'
    return result


def udp_query(data, ip='127.0.0.53'):
    with socket.socket(socket.AF_INET6 if ':' in ip else socket.AF_INET, socket.SOCK_DGRAM) as stream:
        stream.settimeout(4)
        stream.sendto(data, (ip, 53))
        result, source = stream.recvfrom(65536)
        assert source[:2] == (ip, 53) and result[:2] == data[:2]
        return result


query = named('envwrap.test')
for ip in ['198.51.100.53', '2001:db8::53', '127.0.0.53', '::1']:
    assert udp_query(query, ip)[-4:] == socket.inet_aton('198.51.100.123')
private = named('service.INTERNAL.test')
assert udp_query(private)[-4:] == socket.inet_aton('198.51.100.124')
for name in ['notinternal.test', 'internal.test.public.test']:
    assert udp_query(named(name))[-4:] == socket.inet_aton('198.51.100.123')
large = named('large.envwrap.test')
response = udp_query(large)
assert struct.unpack('!H', response[2:4])[0] & 0x200 and len(response) <= 512
response = udp_query(named('large.envwrap.test', 4096))
assert not struct.unpack('!H', response[2:4])[0] & 0x200 and struct.unpack('!H', response[6:8])[0] == 80
# A maximal EDNS advertisement must not kill IPv4 UDP reply injection.
response = udp_query(named('huge.envwrap.test', 65535))
assert len(response) <= 65507 and struct.unpack('!H', response[2:4])[0] & 0x200
assert udp_query(query)[-4:] == socket.inet_aton('198.51.100.123')
for family, ip in [(socket.AF_INET, '127.0.0.53'), (socket.AF_INET6, '::1')]:
    with socket.socket(family) as stream:
        stream.settimeout(4)
        stream.connect((ip, 53))
        for data, expected in [(query, 123), (private, 124), (large, None), (query, 123)]:
            stream.sendall(struct.pack('!H', len(data)) + data)
            response = exact(stream, struct.unpack('!H', exact(stream, 2))[0])
            assert response[:2] == data[:2]
            if expected:
                assert response[-4:] == socket.inet_aton(f'198.51.100.{expected}')
            else:
                assert len(response) > 512 and not struct.unpack('!H', response[2:4])[0] & 0x200
print('controlled/split DNS, suffix boundaries, EDNS size/TC and persistent TCP: PASS', flush=True)
assert {result[4][0] for result in socket.getaddrinfo('envwrap.test', 443, socket.AF_INET, socket.SOCK_STREAM)} == {'198.51.100.123'}
assert {result[4][0] for result in socket.getaddrinfo('service.INTERNAL.test', 443, socket.AF_INET, socket.SOCK_STREAM)} == {'198.51.100.124'}

for family, ip, key in [(socket.AF_INET, '127.0.0.1', 'udp4'), (socket.AF_INET6, '::1', 'udp6'),
                        (socket.AF_INET, '192.168.65.1', 'udplan'), (socket.AF_INET6, 'fdc5:1234::1', 'udplan6')]:
    for size in [0, 1200, 4096, 65507 if family == socket.AF_INET else 65527]:
        with socket.socket(family, socket.SOCK_DGRAM) as stream:
            stream.settimeout(4)
            data = (bytes(range(256)) * ((size + 255) // 256))[:size]
            stream.sendto(data, (ip, ports[key]))
            response, source = stream.recvfrom(65536)
            assert response == data and source[:2] == (ip, ports[key]), (family, size, len(response))
for family, ip in [(socket.AF_INET, '127.0.0.1'), (socket.AF_INET6, '::1')]:
    listener = socket.socket(family, socket.SOCK_DGRAM)
    listener.bind((ip, 0))
    listener.settimeout(4)
    address = listener.getsockname()[:2]
    sizes = [0, 4096, 65507 if family == socket.AF_INET else 65527]

    def local_udp(listener=listener):
        with listener:
            for _ in range(3):
                data, source = listener.recvfrom(65536)
                listener.sendto(data, source)

    thread = threading.Thread(target=local_udp)
    thread.start()
    with socket.socket(family, socket.SOCK_DGRAM) as stream:
        stream.settimeout(4)
        for size in sizes:
            payload = bytes([73]) * size
            stream.sendto(payload, address)
            data, source = stream.recvfrom(65536)
            assert data == payload and source[:2] == address
    thread.join(4)
    assert not thread.is_alive()
for family, ip in [(socket.AF_INET, '203.0.113.9'), (socket.AF_INET6, '2001:db8::9')]:
    with socket.socket(family, socket.SOCK_DGRAM) as wildcard, socket.socket(family, socket.SOCK_DGRAM) as stream:
        wildcard.bind(('0.0.0.0' if family == socket.AF_INET else '::', 9999))
        wildcard.settimeout(.3)
        stream.settimeout(.3)
        stream.sendto(b'blocked', (ip, 9999))
        try:
            stream.recv(64)
            raise AssertionError('external UDP escaped')
        except (TimeoutError, ConnectionRefusedError):
            pass
        try:
            wildcard.recv(64)
            raise AssertionError('child wildcard socket intercepted a nonlocal destination')
        except TimeoutError:
            pass
print('direct UDP zero/full-size datagrams v4/v6 and external UDP blocking: PASS', flush=True)

listeners = []
for family, ip in [(socket.AF_INET, '127.0.0.1'), (socket.AF_INET6, '::1')]:
    listener = socket.socket(family)
    listener.bind((ip, 0))
    listener.listen()
    listener.settimeout(5)
    listeners.append(listener)
callback_file = pathlib.Path(os.environ['TEST_CALLBACK'])
callback_file.with_suffix('.tmp').write_text(json.dumps([s.getsockname()[:2] for s in listeners]))
callback_file.with_suffix('.tmp').rename(callback_file)
for listener in listeners:
    with listener:
        stream, _ = listener.accept()
        with stream:
            stream.sendall(exact(stream, 13))
print('host-to-child localhost callbacks v4/v6: PASS', flush=True)

with socket.create_connection(('203.0.113.9', 444), timeout=3) as stream:
    assert stream.recv(1) == b'X'
deadline = time.monotonic() + 4
while not pathlib.Path(os.environ['TEST_STOPPED']).exists():
    assert time.monotonic() < deadline
    time.sleep(.01)
for ip in ['203.0.113.9', '2001:db8::9']:
    with socket.socket(socket.AF_INET6 if ':' in ip else socket.AF_INET) as stream:
        stream.settimeout(2)
        stream.connect((ip, 443))
        stream.sendall(b'must-not-escape')
        try:
            assert stream.recv(64) == b''
        except ConnectionError:
            pass
tcp_roundtrip(('127.0.0.1', ports['tcp4']))
print('proxy failure: external TCP fails closed, declared localhost still works: PASS', flush=True)
