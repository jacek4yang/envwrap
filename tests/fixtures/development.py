"""Development compatibility with actual OpenSSH agent and Git programs."""
import socket
import subprocess
import time


def run(binary, env, workspace):
    agent_path = workspace / 'openssh-agent.sock'
    environment = dict(env, SSH_AUTH_SOCK=str(agent_path))
    agent = subprocess.Popen(['ssh-agent', '-D', '-a', str(agent_path)],
                             env={'PATH': env['PATH'], 'HOME': env['HOME']},
                             stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while not agent_path.exists():
            assert agent.poll() is None, agent.stderr.read()
            assert time.monotonic() < deadline
            time.sleep(.02)
        native = subprocess.run(['ssh-add', '-l'], env=environment, capture_output=True, timeout=5)
        wrapped = subprocess.run([binary, 'run', '--', 'ssh-add', '-l'], env=environment,
                                 capture_output=True, timeout=10)
        # OpenSSH exit 1 = a reachable agent with no identities; 2 = no agent.
        assert native.returncode == wrapped.returncode == 1, (wrapped.returncode, wrapped.stderr)
    finally:
        agent.terminate()
        agent.communicate(timeout=5)
        agent_path.unlink(missing_ok=True)
    print('real OpenSSH agent protocol through preserved SSH_AUTH_SOCK: PASS')

    git_environment = dict(env, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
    def git(*arguments):
        result = subprocess.run(['git', *arguments], env=git_environment, cwd=workspace,
                                capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        return result.stdout

    git('init', '-q', 'source')
    (workspace / 'source/payload').write_bytes(b'original Git object content\x00\xff')
    git('-C', 'source', 'add', 'payload')
    git('-C', 'source', '-c', 'user.name=envwrap-test', '-c', 'user.email=test@envwrap.invalid',
        '-c', 'commit.gpgsign=false', 'commit', '-qm', 'local acceptance object')
    revision = git('-C', 'source', 'rev-parse', 'HEAD')
    git('clone', '-q', '--bare', 'source', 'origin.git')
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    daemon = subprocess.Popen(['git', 'daemon', '--reuseaddr', '--export-all',
                               f'--base-path={workspace}', '--listen=127.0.0.1', f'--port={port}', str(workspace)],
                              env=git_environment, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while True:
            assert daemon.poll() is None, daemon.stderr.read()
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.2):
                    break
            except ConnectionRefusedError:
                assert time.monotonic() < deadline
                time.sleep(.02)
        result = subprocess.run([binary, 'run', '--', 'git', 'clone', '-q',
                                 f'git://127.0.0.1:{port}/origin.git', 'checkout'],
                                env=git_environment, cwd=workspace, capture_output=True, timeout=15)
        assert result.returncode == 0, result.stderr
        assert git('-C', 'checkout', 'rev-parse', 'HEAD') == revision
        assert (workspace / 'checkout/payload').read_bytes() == (workspace / 'source/payload').read_bytes()
    finally:
        daemon.terminate()
        daemon.communicate(timeout=5)
    print('real Git clone over transparent localhost TCP, objects and worktree writes: PASS')
