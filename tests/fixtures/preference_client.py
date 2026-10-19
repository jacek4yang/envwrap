"""Address-family preference preserves single-stack names and both DNS transports."""
import socket
import struct
import sys

preferred = 1 if sys.argv[1] == 'ipv4' else 28
for tcp in (False, True):
    for host in ('dual.test', 'v4only.test', 'v6only.test'):
        for kind in (1, 28):
            query = struct.pack('!6H', 234, 0x100, 1, 0, 0, 0)
            query += b''.join(bytes([len(label)]) + label.encode() for label in host.split('.'))
            query += struct.pack('!BHH', 0, kind, 1)
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM if tcp else socket.SOCK_DGRAM) as stream:
                stream.settimeout(3)
                stream.connect(('198.18.0.1', 53))
                if tcp:
                    stream.sendall(struct.pack('!H', len(query)) + query)
                    with stream.makefile('rb') as reader:
                        length = struct.unpack('!H', reader.read(2))[0]
                        result = reader.read(length)
                else:
                    stream.send(query)
                    result = stream.recv(4096)
            expected = int(kind == (preferred if host == 'dual.test' else 1 if host == 'v4only.test' else 28))
            assert result[:2] == query[:2] and result[3] & 15 == 0
            assert struct.unpack('!H', result[6:8])[0] == expected, (host, kind, preferred, result)
            if expected:
                family = socket.AF_INET if kind == 1 else socket.AF_INET6
                expected_ip = '198.51.100.123' if kind == 1 else '2001:db8::123'
                assert result.endswith(socket.inet_pton(family, expected_ip))
print(f'{sys.argv[1]} DNS preference: dual-stack selection, single-stack preservation, TCP/UDP PASS', flush=True)
