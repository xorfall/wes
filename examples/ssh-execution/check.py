#!/usr/bin/env python3
"""Exercise the actual recipe and source in an isolated home. Optional real OpenSSH daemon."""
import argparse
import getpass
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--real-openssh', action='store_true')
    parser.add_argument('--terminal', action='store_true', help='Exercise the terminal-only recipe through the application PTY API')
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="wes ssh '") as directory:
        root = Path(directory)
        recipe = json.loads((HERE / ('terminal.yaml' if args.terminal else 'environments.yaml')).read_text())
        target = recipe['targets']['remote']
        target.update(cwd=str(root), identity_file=str(root / 'identity'), known_hosts=str(root / 'known_hosts'))
        target['env'].update(HOME=str(root), ZDOTDIR=str(root), ENV=str(root / 'empty-env'), SHELL='/bin/sh')
        daemon = None
        log = None
        try:
            if args.real_openssh:
                ssh = shutil.which('ssh')
                sshd = shutil.which('sshd') or '/usr/sbin/sshd'
                keygen = shutil.which('ssh-keygen')
                assert ssh and keygen and Path(sshd).is_file(), 'Install OpenSSH client, server and keygen'
                for key in ('identity', 'host_key'):
                    subprocess.run([keygen, '-q', '-t', 'ed25519', '-N', '', '-f', str(root / key)], check=True)
                (root / 'authorized_keys').write_text((root / 'identity.pub').read_text())
                (root / 'authorized_keys').chmod(0o600)
                with socket.socket() as port_socket:
                    port_socket.bind(('127.0.0.1', 0))
                    port = port_socket.getsockname()[1]
                target.update(client=ssh, port=port, user=getpass.getuser())
                public = (root / 'host_key.pub').read_text().split()
                (root / 'known_hosts').write_text(f'[127.0.0.1]:{port} {public[0]} {public[1]}\n')
                # Explicit isolated files only; no system sshd config or user authorized_keys.
                def config_path(name):
                    return '"' + str(root / name).replace('\\', '\\\\').replace('"', '\\"') + '"'
                config = root / 'sshd_config'
                config.write_text(f'''Port {port}
ListenAddress 127.0.0.1
HostKey {config_path('host_key')}
PidFile {config_path('sshd.pid')}
AuthorizedKeysFile {config_path('authorized_keys')}
StrictModes yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitUserRC no
SetEnv HOME={config_path('.')} ZDOTDIR={config_path('.')} SHELL=/bin/sh ENV={config_path('empty-env')}
AllowUsers {target['user']}
AllowAgentForwarding no
AllowTcpForwarding no
X11Forwarding no
PermitTunnel no
LogLevel VERBOSE
''')
                log = (root / 'sshd.log').open('w+')
                daemon = subprocess.Popen([sshd, '-D', '-e', '-f', str(config)], stdout=log, stderr=log)
                for _ in range(100):
                    if daemon.poll() is not None:
                        log.seek(0)
                        raise AssertionError('Synthetic sshd could not start: ' + log.read())
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    raise AssertionError('Synthetic sshd did not start within five seconds')
            else:
                # The fake client does not authenticate, but planning still verifies
                # that its explicitly selected files exist before dispatch.
                (root / 'identity').write_text('synthetic identity fixture\n')
                (root / 'identity').chmod(0o600)
                (root / 'known_hosts').write_text('synthetic host fixture\n')
                client = root / 'client'
                client.write_text('#!/bin/sh\nprintf "%s\\0" "$@" >> ' + shlex.quote(str(root / 'argv')) + '\nfor last; do :; done\nexec /bin/sh -c "$last"\n')
                client.chmod(0o700)
                target['client'] = str(client)
            if args.terminal:
                from terminal_check import check_terminal
                check_terminal(binary, root, recipe, args.real_openssh)
                return
            recipe_file = root / 'environments.yaml'
            recipe_file.write_text(json.dumps(recipe))
            command = [str(binary), '--home', str(root / 'home'), '--env-file', str(recipe_file), '--env', 'ssh_demo', '--file', str(HERE / 'run.wes')]
            result = subprocess.run(command, capture_output=True, text=True, timeout=45)
            if result.returncode:
                detail = result.stderr
                if log:
                    log.flush(); log.seek(0); detail += '\n' + log.read()
                raise AssertionError(detail)
            values = [json.loads(line.split(': ', 1)[1]) for line in result.stdout.splitlines()]
            outputs = [value for value in values if isinstance(value, dict) and 'exitCode' in value]
            assert len(outputs) == 3, values
            # Bytes use the codec's base64 JSON representation.
            import base64
            def stdout(value):
                data = value['stdout']
                return base64.b64decode(data).decode() if isinstance(data, str) else bytes(data).decode()
            assert all(value['exitCode'] == 0 for value in outputs), outputs
            assert stdout(outputs[0]) == "literal ; $(touch SHOULD_NOT_EXIST) $HOME 'quote'\n", outputs
            assert stdout(outputs[1]) == "literal '$HOME; value\n", outputs
            assert Path(stdout(outputs[2]).strip()).resolve() == root.resolve(), outputs
            assert not (root / 'SHOULD_NOT_EXIST').exists()
            if args.real_openssh:
                # Wrong pinned host key must refuse without running the remote command.
                (root / 'known_hosts').write_text('')
                rejected = subprocess.run([str(binary), '--home', str(root / 'refused'), '--env-file', str(recipe_file), '--env', 'ssh_demo', '--file', str(HERE / 'run.wes')], capture_output=True, text=True, timeout=45)
                assert rejected.returncode != 0 and 'ENV036' in rejected.stderr, rejected.stderr
            else:
                argv = (root / 'argv').read_bytes().split(b'\0')
                for flag in (b'StrictHostKeyChecking=yes', b'IdentityAgent=none', b'ControlPath=none', b'ConnectionAttempts=1'):
                    assert argv.count(flag) == 3, (flag, argv)
        finally:
            if daemon:
                daemon.terminate()
                try:
                    daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    daemon.kill(); daemon.wait(timeout=5)
            if log:
                log.close()
    print('PASS ssh-execution: actual recipe/source, literal arguments, cwd/env, ' + ('OpenSSH key authentication and host verification' if args.real_openssh else 'synthetic client and explicit launch options'))

if __name__ == '__main__':
    main()
