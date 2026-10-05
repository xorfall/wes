#!/usr/bin/env python3
"""Shell baseline: explicit Unix-socket Docker API, no Docker CLI installation required."""
import http.client, json, socket, struct, sys

class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__('localhost', timeout=10)
        self.path = path
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)

def get(path):
    client = UnixHTTP(sys.argv[2])
    try:
        client.request('GET', path)
        response = client.getresponse()
        assert response.status == 200, response.status
        return response.read()
    finally:
        client.close()

version = json.loads(get('/version'))
inventory = json.loads(get('/v1.45/containers/json?all=true&filters=%7B%22label%22%3A%5B%22com.docker.compose.project%3Dwes-investigation%22%5D%7D'))
identity = inventory[0]['Id']
info = json.loads(get(f'/v1.45/containers/{identity}/json'))
wire = get(f'/v1.45/containers/{identity}/logs?stdout=true&stderr=true&timestamps=true&tail=200')
lines = []
while wire:
    channel, length = struct.unpack('>BxxxI', wire[:8])
    payload, wire = wire[8:8+length], wire[8+length:]
    lines.append(payload.decode().split(' ', 1)[1].rstrip('\n'))
if sys.argv[1] == 'raw':
    result = {'inventory': inventory, 'inspection': info, 'logs': lines}
else:
    failures = [line for line in lines if 'allocation failed' in line]
    result = dict(name=info['Name'].lstrip('/'), exit_code=info['State']['ExitCode'],
                  oom_killed=info['State']['OOMKilled'], restarts=info['RestartCount'],
                  lines=len(lines), allocation_failures=len(failures), truncated=False,
                  examples=failures[:2])
print(json.dumps(result, ensure_ascii=False, separators=(',', ':')))
