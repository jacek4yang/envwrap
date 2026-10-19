"""Real PTY and process-tree lifecycle checks, used by network acceptance."""
import fcntl
import os
import pathlib
import pty
import select
import shutil
import signal
import socket
import struct
import subprocess
import termios
import time


def alive(pid):
    try:
        # A zombie cannot execute or hold any network/file descriptor.
        return pathlib.Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1][0] != 'Z'
    except FileNotFoundError:
        return False


def run(binary, env, workspace):
    for actor, termination in [('launcher', signal.SIGTERM), ('launcher', signal.SIGKILL), ('helper', signal.SIGKILL)]:
        path = workspace / f'lifecycle-{actor}-{termination}.sock'
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(path))
        listener.listen()
        listener.settimeout(8)
        code = r'''
import os,socket,subprocess,sys,time
for name in os.listdir('/proc/self/fd'):
    if int(name) > 2:
        try:
            os.fstat(int(name))
        except OSError:
            continue
        raise AssertionError('inherited runtime descriptor')
subprocess.Popen(['/bin/sleep','60'])
with socket.socket(socket.AF_UNIX) as stream:
    stream.connect(sys.argv[1])
    stream.sendall(b'ready')
time.sleep(60)
'''
        child = subprocess.Popen([binary, '/usr/bin/python3', '-c', code, str(path)], env=env,
                                 cwd=workspace, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        runtime_directories = set()
        try:
            with listener:
                stream, _ = listener.accept()
                with stream:
                    pid, uid, _ = struct.unpack('3i', stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    assert uid == os.getuid()
                    assert stream.recv(5) == b'ready'
            descendants = [int(value) for value in pathlib.Path(f'/proc/{pid}/task/{pid}/children').read_text().split()]
            assert descendants and all(alive(value) for value in [pid, *descendants])
            for process in pathlib.Path(f'/proc/{child.pid}/task/{child.pid}/children').read_text().split():
                for argument in pathlib.Path(f'/proc/{process}/cmdline').read_bytes().split(b'\0'):
                    if argument.startswith(b'/tmp/envwrap-') and argument.endswith(b'/control.sock'):
                        runtime_directories.add(pathlib.Path(os.fsdecode(argument)).parent)
            if actor == 'launcher':
                child.send_signal(termination)
            else:
                status = pathlib.Path(f'/proc/{pid}/status').read_text()
                helper = int(next(line.split()[1] for line in status.splitlines() if line.startswith('PPid:')))
                os.kill(helper, termination)
            stdout, stderr = child.communicate(timeout=5)
            if actor == 'launcher':
                assert child.returncode == (143 if termination == signal.SIGTERM else -signal.SIGKILL), (child.returncode, stdout, stderr)
            else:
                assert child.returncode != 0, 'helper loss was reported as success'
            deadline = time.monotonic() + 5
            while any(alive(value) for value in [pid, *descendants]):
                assert time.monotonic() < deadline, 'orphan application/descendant after launcher exit'
                time.sleep(.02)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)
            path.unlink(missing_ok=True)
            for directory in runtime_directories:
                if directory.exists():
                    shutil.rmtree(directory)
    print('SIGTERM/SIGKILL/helper loss: status, descriptor closure and process-tree cleanup: PASS')

    code = r'''
import os,signal,sys,time
assert all(os.isatty(fd) for fd in [0,1,2])
def resized(signum,frame):
    size=os.get_terminal_size()
    print(f'SIZE:{size.columns}x{size.lines}',flush=True)
def interrupted(signum,frame):
    print('INTERRUPTED',flush=True)
    sys.exit(23)
signal.signal(signal.SIGWINCH,resized)
signal.signal(signal.SIGINT,interrupted)
print('TTY-READY',flush=True)
assert input()=='interactive-input'
print('INPUT-OK',flush=True)
time.sleep(60)
'''
    argv = [binary, '/usr/bin/python3', '-c', code]
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(workspace)
        os.execve(binary, argv, env)
    output = bytearray()

    def until(marker):
        deadline = time.monotonic() + 10
        while marker not in output:
            assert time.monotonic() < deadline, bytes(output)
            if select.select([fd], [], [], .1)[0]:
                try:
                    data = os.read(fd, 65536)
                except OSError as error:
                    raise AssertionError((bytes(output), error)) from error
                assert data, bytes(output)
                output.extend(data)

    waited = False
    try:
        until(b'TTY-READY')
        os.write(fd, b'interactive-input\n')
        until(b'INPUT-OK')
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 37, 101, 0, 0))
        until(b'SIZE:101x37')
        os.write(fd, b'\x03')
        until(b'INTERRUPTED')
        deadline = time.monotonic() + 5
        while True:
            result, status = os.waitpid(pid, os.WNOHANG)
            if result:
                waited = True
                assert os.waitstatus_to_exitcode(status) == 23, (status, bytes(output))
                break
            assert time.monotonic() < deadline, 'terminal child did not exit'
            time.sleep(.02)
    finally:
        if not waited:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(fd)
    print('actual PTY: stdin/stdout/stderr, interactive input, SIGWINCH size, Ctrl-C and exit status: PASS')
