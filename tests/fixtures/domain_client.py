"""Exercise proxy-side names using real TCP services, without proxy-aware APIs."""
import http.client
import ipaddress
import os
from pathlib import Path
import socket
import struct
import time


def query(name, kind, tcp=False):
    data = struct.pack('!6H', 123, 0x100, 1, 0, 0, 0)
    data += b''.join(bytes([len(label)]) + label.encode() for label in name.split('.'))
    data += struct.pack('!BHH', 0, kind, 1)
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM if tcp else socket.SOCK_DGRAM) as stream:
        stream.settimeout(3)
        stream.connect(('198.18.0.1', 53))
        if tcp:
            stream.sendall(struct.pack('!H', len(data)) + data)
            with stream.makefile('rb') as reader:
                size = struct.unpack('!H', reader.read(2))[0]
                result = reader.read(size)
        else:
            stream.send(data)
            result = stream.recv(4096)
    assert result[:2] == data[:2] and result[3] & 15 == 0
    assert not (struct.unpack('!H', result[2:4])[0] & 0x20), 'synthetic DNS must not claim DNSSEC authentication'
    return result


allocated = set()
for host, expected in [('one.example', b'first origin'), ('two.example', b'second origin')]:
    for family, kind, width, network in [(socket.AF_INET, 1, 4, '198.19.0.0/16'),
                                        (socket.AF_INET6, 28, 16, 'fd23:656e:7677:1::/64')]:
        result = query(host, kind)
        address = socket.inet_ntop(family, result[-width:])
        assert ipaddress.ip_address(address) in ipaddress.ip_network(network)
        assert query(host.upper(), kind, tcp=True)[-width:] == result[-width:]
        allocated.add(address)
        with socket.socket(family) as stream:
            stream.settimeout(4)
            stream.connect((address, 80))
            stream.sendall(f'GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n'.encode())
            response = http.client.HTTPResponse(stream)
            response.begin()
            assert response.status == 200 and response.read() == expected
        native = {item[4][0] for item in socket.getaddrinfo(host, 80, family, socket.SOCK_STREAM)}
        assert native == {address}
assert len(allocated) == 4, 'distinct domains and families must not collide'
for kind in (64, 65):
    assert query('one.example', kind)[6:8] == b'\x00\x00', 'SVCB/HTTPS hints bypass domain routing'
assert query('service.INTERNAL.test', 1)[-4:] == socket.inet_aton('198.51.100.124')
# A literal and an unresolvable domain must not accidentally reach a valid origin.
missing = socket.inet_ntoa(query('missing.example', 1)[-4:])
for target in (missing, '198.19.255.255', 'fd23:656e:7677:1::ffff'):
    with socket.create_connection((target, 80), timeout=3) as stream:
        stream.sendall(b'GET / HTTP/1.0\r\n\r\n')
        try:
            assert stream.recv(1) == b''
        except ConnectionError:
            pass
Path('application-write').write_text('original filesystem')
print('socks5h: distinct origins via A/AAAA, native getaddrinfo, TCP/UDP DNS, case folding, internal DNS, IP hints and unknown-domain refusal PASS', flush=True)
# Trigger the fixture proxy outage, then let the parent verify startup refusal.
with socket.create_connection(('203.0.113.9', 444), timeout=3) as stream:
    assert stream.recv(1) == b'X'
deadline = time.monotonic() + 3
while not Path(os.environ['TEST_STOPPED']).exists():
    assert time.monotonic() < deadline
    time.sleep(.01)
