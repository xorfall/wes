"""Synthetic executable protocol peer. Uses only local pipes and files."""
import json
import os
from pathlib import Path
import struct
import sys

mode, directory = sys.argv[1:]
root = Path(directory)
with (root / "attempts").open("a") as file:
    file.write("started\n")
(root / "pid").write_text(str(os.getpid()))
if mode == "no-read":
    import threading
    threading.Event().wait()


def read():
    header = sys.stdin.buffer.read(4)
    if len(header) != 4:
        raise EOFError()
    size = struct.unpack(">I", header)[0]
    if not 0 < size <= 1048576:
        raise ValueError("bad frame size")
    body = sys.stdin.buffer.read(size)
    if len(body) != size:
        raise EOFError()
    return json.loads(body)


start = read()
assert start["version"] == 1
assert not os.environ.get("WES_SESSION_UNGRANTED_ENV")
nonce = start["session"]
(root / "handoff").write_text("received\n")


def send(message, session=nonce):
    body = json.dumps({"version": 1, "session": session, "message": message}).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()


if mode == "before-ready":
    send({"kind": "event", "sequence": 1, "data": 7})
elif mode == "foreign":
    send({"kind": "ready"}, "00000000-0000-0000-0000-000000000000")
elif mode == "oversized":
    sys.stdout.buffer.write(struct.pack(">I", 1048577))
    sys.stdout.buffer.flush()
elif mode == "stderr":
    sys.stderr.write("synthetic-session-private-diagnostic " * 3000)
    sys.stderr.flush()
else:
    send({"kind": "ready"})
    if mode == "credentials":
        assert start["credentials"] == {"Token": "synthetic-provider-session-secret"}
        assert start["endpoint"] == "https://127.0.0.1:58443"
        assert "Token" not in os.environ and "WES_TOKEN" not in os.environ
        send({"kind": "result", "data": None})
        send({"kind": "end"})
    elif mode == "finite-eof":
        pass  # Start received, no receipt: the external outcome is unknown.
    elif mode == "finite-failed":
        send({"kind": "failed"})
    elif mode == "finite-result-no-end":
        send({"kind": "result", "data": None})
    elif mode in ["finite", "nullable"]:
        send({"kind": "result", "data": 9007199254740993 if mode == "finite" else None})
        send({"kind": "end"})
    elif mode in ["bad-sequence", "wrong-type"]:
        send({"kind": "event", "sequence": 2 if mode == "bad-sequence" else 1,
              "data": "bad" if mode == "wrong-type" else 7})
    elif mode == "stream":
        assert start["event_credit"] == 1
        send({"kind": "event", "sequence": 1, "data": 7})
        credit = read()
        assert credit["session"] == nonce and credit["message"] == {"kind": "credit", "count": 1}
        os.close(0)  # A valid peer may finish without reading the final replenishment credit.
        send({"kind": "event", "sequence": 2, "data": 8})
        send({"kind": "end"})
    elif mode == "hold":
        while True:
            read()
    elif mode in ["control", "private-control", "control-eof"]:
        assert start["credentials"] == {"Token": "synthetic-provider-session-secret"}
        while True:
            frame = read()
            assert frame["session"] == nonce and frame["version"] == 1
            message = frame["message"]
            if message["kind"] == "credit":
                continue
            assert message["kind"] == "control"
            action = message["data"]
            with (root / "actions").open("a") as file:
                file.write(str(action) + "\n")
            if mode == "control-eof":
                break
            if action == 10:
                continue  # Dispatched external work with no known receipt; host must not replay.
            outcome = {"kind": "refused" if action == 8 else "unknown" if action == 9 else "confirmed"}
            if outcome["kind"] == "confirmed":
                outcome["data"] = "synthetic-private-control-body" if mode == "private-control" else True
            send({"kind": "control", "id": message["id"], "outcome": outcome})
    else:
        raise ValueError("unknown fixture mode")
