#!/usr/bin/env python3
"""Local Unix dumpcap-compatible session; original raw travels only over credited pipes."""
import base64
import hashlib
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import selectors
import re
import signal
import stat
import struct
import subprocess
import sys
import time

SPEC = importlib.util.spec_from_file_location("capture_container", Path(__file__).resolve().parent.parent / "tracing-offline/capture_adapter.py")
container = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(container)
FRAME_LIMIT = 262144
CHUNK = 65536
STDERR_LIMIT = 65536
LINE_LIMIT = 4096
STOP_SECONDS = 2.0
PIPE_SECONDS = 0.5
STATUS_SECONDS = 0.25


class Refused(ValueError):
    pass


def unique(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise Refused("duplicate key")
        value[key] = item
    return value


def decode(data):
    def nonfinite(_):
        raise Refused("nonfinite number")
    return json.loads(data, object_pairs_hook=unique, parse_constant=nonfinite)


def read_start(input):
    def exact(size):
        data = bytearray()
        while len(data) < size:
            part = os.read(input.fileno(), size - len(data))
            if not part:
                raise Refused("truncated start")
            data.extend(part)
        return bytes(data)
    length = exact(4)
    size = struct.unpack(">I", length)[0]
    if not 1 <= size <= FRAME_LIMIT:
        raise Refused("start size")
    return decode(exact(size))


def bounded_text(value, limit, whitespace=False):
    if not isinstance(value, str) or not 1 <= len(value) <= limit:
        return False
    return all(ord(c) >= 32 and ord(c) != 127 or whitespace and c in "\t\r\n" for c in value)


def profile(value):
    keys = {"name", "backend", "interface", "filter", "durationMs", "maxBytes", "packets", "snapLength", "promiscuous"}
    if not isinstance(value, dict) or set(value) != keys:
        raise Refused("profile fields")
    for key, size in (("name", 64), ("backend", 4096), ("interface", 1024), ("filter", 4096)):
        if not bounded_text(value[key], size, key == "filter"):
            raise Refused("profile text")
    for key, low, high in (("durationMs", 1, 120000), ("maxBytes", 24, 536870912), ("packets", 1, 1000000), ("snapLength", 1, 65535)):
        if type(value[key]) is not int or not low <= value[key] <= high:
            raise Refused("profile budget")
    if type(value["promiscuous"]) is not bool or not os.path.isabs(value["backend"]):
        raise Refused("explicit backend required")
    # No pipe input, network capture protocol, implicit discovery or option-shaped interface.
    iface = value["interface"]
    if iface.startswith("-") or any(c in iface for c in "/\\:@"):
        raise Refused("explicit local interface required")
    if os.name != "posix" or not stat.S_ISREG(os.stat(value["backend"]).st_mode) or not os.access(value["backend"], os.X_OK):
        raise Refused("local Unix executable required")
    return dict(value)


class Predicate:
    """A bounded pure libpcap subset, with explicit qualifiers and no DNS."""
    token = re.compile(r" +|&&|\|\||[()!]|[A-Za-z0-9_.:/-]+")

    def __init__(self, text):
        if (not isinstance(text, str) or not 1 <= len(text) <= 4096
                or not text.isascii() or any(ord(c) < 32 or ord(c) == 127 for c in text)):
            raise Refused("filter text")
        self.tokens, at = [], 0
        while at < len(text):
            match = self.token.match(text, at)
            if match is None:
                raise Refused("unsupported filter token")
            item, at = match.group(), match.end()
            if item.isspace():
                continue
            self.tokens.append(item)
            if len(self.tokens) > 256:
                raise Refused("filter work limit")
        self.at = 0

    def peek(self):
        return self.tokens[self.at] if self.at < len(self.tokens) else None

    def take(self):
        value = self.peek()
        if value is None:
            raise Refused("incomplete filter")
        self.at += 1
        return value

    @staticmethod
    def port(value):
        if not re.fullmatch(r"0|[1-9][0-9]{0,4}", value) or int(value) > 65535:
            raise Refused("numeric filter port")
        return value

    def primitive(self):
        words = []
        protocol = self.peek()
        if protocol in {"tcp", "udp", "ip", "ip6"}:
            words.append(self.take())
            if self.peek() not in {"src", "dst", "port", "portrange"}:
                return words[0]
            if protocol not in {"tcp", "udp"}:
                raise Refused("unsupported protocol qualifier")
        if self.peek() in {"src", "dst"}:
            words.append(self.take())
        kind = self.take()
        if kind not in {"host", "net", "port", "portrange"} or (protocol in {"tcp", "udp"} and kind not in {"port", "portrange"}):
            raise Refused("explicit filter qualifier required")
        value = self.take()
        if kind == "port":
            value = self.port(value)
        elif kind == "portrange":
            ends = value.split("-")
            if len(ends) != 2 or int(self.port(ends[0])) > int(self.port(ends[1])):
                raise Refused("ordered numeric port range")
        else:
            try:
                if kind == "host":
                    value = str(ipaddress.ip_address(value))
                else:
                    if "/" not in value or not re.fullmatch(r"0|[1-9][0-9]{0,2}", value.rsplit("/", 1)[1]):
                        raise Refused("explicit numeric CIDR required")
                    value = str(ipaddress.ip_network(value, strict=True))
            except ValueError:
                raise Refused("numeric filter address required") from None
        return " ".join([*words, kind, value])

    def unary(self, depth):
        if depth > 32:
            raise Refused("filter nesting limit")
        if self.peek() in {"not", "!"}:
            self.take()
            return "(not " + self.unary(depth + 1) + ")"
        if self.peek() == "(":
            self.take()
            result = self.expression(depth + 1)
            if self.take() != ")":
                raise Refused("filter grouping")
            return result
        return self.primitive()

    def expression(self, depth):
        # libpcap gives AND and OR equal precedence, associated left to right.
        result = self.unary(depth)
        while self.peek() in {"and", "or", "&&", "||"}:
            operator = self.take()
            operator = "and" if operator in {"and", "&&"} else "or"
            result = "(" + result + " " + operator + " " + self.unary(depth) + ")"
            if len(result) > 4096:
                raise Refused("filter output limit")
        return result

    def render(self):
        result = self.expression(0)
        if self.peek() is not None or len(result) > 4096:
            raise Refused("unsupported filter expression")
        return result


NARROWING = {"duration_ms": ("durationMs", 1), "packet_limit": ("packets", 1), "max_bytes": ("maxBytes", 24)}
OPTION_NAMES = {*NARROWING, "filter_condition"}


def narrow_profile(frozen, arguments):
    effective = dict(frozen)
    for parameter, (field, minimum) in NARROWING.items():
        if parameter in arguments:
            value = arguments[parameter]
            if type(value) is not int or not minimum <= value <= frozen[field]:
                raise Refused("profile narrowing ceiling")
            effective[field] = value
    if "filter_condition" in arguments:
        base = Predicate(frozen["filter"]).render()
        condition = Predicate(arguments["filter_condition"]).render()
        effective["filter"] = "(" + base + " and " + condition + ")"
        if len(effective["filter"]) > 4096:
            raise Refused("combined filter limit")
    return effective


def start_profile(start):
    keys = {"version", "session", "run", "operation", "arguments", "endpoint", "credentials", "contracts", "event_credit", "recording"}
    if not isinstance(start, dict) or set(start) != keys or type(start["version"]) is not int or start["version"] != 1:
        raise Refused("start contract")
    if not bounded_text(start["session"], 128) or not bounded_text(start["run"], 128) or start["operation"] != ["start"]:
        raise Refused("start identity")
    if start["endpoint"] is not None or start["credentials"] != {} or type(start["event_credit"]) is not int or start["event_credit"] != 1:
        raise Refused("start authority")
    contracts, recording = start["contracts"], start["recording"]
    if not isinstance(contracts, dict) or set(contracts) != {"result", "parameters", "control"}:
        raise Refused("typed contracts required")
    parameters = contracts["parameters"]
    if not isinstance(parameters, dict) or "profile" not in parameters or not set(parameters) <= {"profile", *OPTION_NAMES}:
        raise Refused("typed profile parameters required")
    if not isinstance(contracts["control"], list) or len(contracts["control"]) != 2 or not all(bounded_text(v, 256) for v in [contracts["result"], *parameters.values(), *contracts["control"]]):
        raise Refused("typed digests required")
    if not isinstance(recording, dict) or set(recording) != {"contract", "byte_field", "max_bytes", "max_duration_ms", "credit"}:
        raise Refused("recording required")
    if not bounded_text(recording["contract"], 256) or recording["byte_field"] != ["raw"] or type(recording["credit"]) is not int or recording["credit"] != 1:
        raise Refused("raw contract")
    arguments = start["arguments"]
    if not isinstance(arguments, dict) or "profile" not in arguments or not set(arguments) <= set(parameters):
        raise Refused("profile argument required")
    frozen = profile(arguments["profile"])
    if type(recording["max_bytes"]) is not int or not frozen["maxBytes"] <= recording["max_bytes"] <= 536870912:
        raise Refused("raw reservation too small")
    if type(recording["max_duration_ms"]) is not int or not frozen["durationMs"] + 3000 <= recording["max_duration_ms"] <= 86400000:
        raise Refused("lifetime reservation too small")
    return narrow_profile(frozen, arguments)


def backend_argv(frozen):
    argv = [frozen["backend"], "-i", frozen["interface"], "-f", frozen["filter"], "-F", "pcap", "-q",
            "-s", str(frozen["snapLength"]), "-c", str(frozen["packets"]),
            "-a", f'duration:{frozen["durationMs"] / 1000:.3f}', "-w", "-"]
    if not frozen["promiscuous"]:
        argv.append("-p")
    return argv


class Prefix:
    """Counts only physically acknowledged original bytes; retains no packet payloads."""
    def __init__(self):
        self.raw_bytes, self.count, self.truncated = 0, 0, 0
        self.digest, self.header = hashlib.sha256(), bytearray()
        self.stage, self.needed, self.valid = "global", 24, True
        self.interface, self.endian, self.power, self.current = None, None, None, None
        self.rows = {"packetCount": "0", "packets": []}

    @property
    def complete(self):
        return self.valid and self.interface is not None and self.stage == "record" and self.needed == 16

    def admit(self, data):
        self.digest.update(data)
        self.raw_bytes += len(data)
        if not self.valid:
            return
        if not data or len(data) > self.needed:
            raise Refused("raw reader exceeded prefix boundary")
        self.needed -= len(data)
        if self.stage != "body":
            self.header.extend(data)
        if self.needed:
            return
        try:
            if self.stage == "global":
                self.endian, self.power, self.interface = container.classic_header(bytes(self.header))
                if self.interface["snapLength"] > 65535:
                    raise container.InvalidCapture("live snapshot length exceeds its bound")
                self.stage, self.needed = "record", 16
            elif self.stage == "record":
                captured, original, ticks = container.classic_packet_header(bytes(self.header), self.endian, self.power, self.interface["snapLength"])
                self.current = (self.raw_bytes - 16, self.raw_bytes, captured, original, ticks)
                self.stage, self.needed = "body", captured
                if captured == 0:
                    self.finish_packet()
            else:
                self.finish_packet()
        except (container.InvalidCapture, struct.error):
            self.valid = False
        self.header.clear()

    def finish_packet(self):
        record, data, captured, original, ticks = self.current
        container.packet(self.rows, self.interface, record, data, captured, original, ticks)
        self.count += 1
        self.truncated += captured < original
        self.stage, self.needed = "record", 16

    def index(self):
        if not self.complete:
            return None
        return {"format": "pcap", "rawBytes": str(self.raw_bytes), "sha256": "sha256:" + self.digest.hexdigest(),
                "packetCount": str(self.count), "indexComplete": self.count <= container.INDEX_PACKETS,
                "skippedBlocks": 0, "interfaces": [self.interface], "packets": self.rows["packets"]}

    def status(self, frozen, phase):
        return {"profile": frozen, "phase": phase, "rawBytes": str(self.raw_bytes), "packetCount": str(self.count),
                "truncatedPackets": str(self.truncated), "drops": None, "containerComplete": self.complete,
                "indexComplete": self.complete and self.count <= container.INDEX_PACKETS, "coverageKnown": False}


class Capture:
    def __init__(self, start, frozen, input, output):
        self.start, self.frozen, self.input, self.output = start, frozen, input, output
        self.prefix, self.selector, self.child = Prefix(), selectors.DefaultSelector(), None
        self.input_bytes, self.stderr_line, self.raw_buffer = bytearray(), bytearray(), bytearray()
        self.stderr_bytes, self.discarded = 0, 0
        self.pending_raw, self.raw_credit, self.event_credit = None, True, True
        self.raw_sequence, self.event_sequence = 0, 0
        self.opened, self.backend_ready, self.host_closed = False, False, False
        self.stdout_eof, self.stderr_eof = False, False
        self.reason, self.stop_until, self.pipe_until, self.forced = None, None, None, False
        self.control, self.dirty = None, False
        self.next_status, self.last_phase = 0.0, None

    def send(self, message):
        data = json.dumps({"version": 1, "session": self.start["session"], "message": message}, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
        if not 1 <= len(data) <= FRAME_LIMIT:
            raise Refused("outgoing frame budget")
        self.output.write(struct.pack(">I", len(data)))
        self.output.write(data)
        self.output.flush()

    def shell(self, phase, reason):
        return {"profile": self.frozen, "summary": self.prefix.status(self.frozen, phase), "reason": reason,
                "index": self.prefix.index() if phase != "opening" else None, "raw": ""}

    def stop(self, reason):
        if self.reason is not None:
            return
        self.reason, self.stop_until, self.dirty = reason, time.monotonic() + STOP_SECONDS, True
        if self.child.poll() is None:
            self.child.send_signal(signal.SIGINT)

    def receive(self, frame):
        if not isinstance(frame, dict) or set(frame) != {"version", "session", "message"} or type(frame["version"]) is not int or frame["version"] != 1 or frame["session"] != self.start["session"]:
            raise Refused("foreign host frame")
        msg = frame["message"]
        if not isinstance(msg, dict):
            raise Refused("host message")
        if set(msg) == {"kind", "count"} and type(msg["count"]) is int and msg["count"] == 1:
            if msg["kind"] == "bytes-credit" and not self.raw_credit and self.pending_raw is not None:
                self.prefix.admit(self.pending_raw)
                self.pending_raw, self.raw_credit, self.dirty = None, True, True
                if self.pipe_until is not None:
                    self.pipe_until = time.monotonic() + PIPE_SECONDS
                if not self.prefix.valid:
                    self.stop("malformed")
                elif self.prefix.count >= self.frozen["packets"]:
                    self.stop("packets")
                elif self.prefix.raw_bytes >= self.frozen["maxBytes"]:
                    self.stop("bytes")
                return
            if msg["kind"] == "credit" and not self.event_credit:
                self.event_credit = True
                return
        if set(msg) == {"kind", "id", "data"} and msg["kind"] == "control" and self.opened and self.control is None and bounded_text(msg["id"], 128):
            if msg["data"] != {"operation": "stop"}:
                self.send({"kind": "control", "id": msg["id"], "outcome": {"kind": "refused"}})
            else:
                self.control = msg["id"]
                self.stop("stopped")
            return
        raise Refused("host credit or control state")

    def stdin_ready(self):
        data = os.read(self.input.fileno(), CHUNK)
        if not data:
            self.host_closed = True
            return
        self.input_bytes.extend(data)
        while len(self.input_bytes) >= 4:
            length = struct.unpack(">I", self.input_bytes[:4])[0]
            if not 1 <= length <= FRAME_LIMIT:
                raise Refused("host frame budget")
            if len(self.input_bytes) < 4 + length:
                break
            self.receive(decode(bytes(self.input_bytes[4:4 + length])))
            del self.input_bytes[:4 + length]
        if len(self.input_bytes) > FRAME_LIMIT + 4:
            raise Refused("host buffer budget")

    def stderr_ready(self):
        data = os.read(self.child.stderr.fileno(), 4096)
        if not data:
            self.stderr_eof = True
            self.selector.unregister(self.child.stderr)
            return
        self.stderr_bytes += len(data)
        if self.pipe_until is not None:
            self.pipe_until = time.monotonic() + PIPE_SECONDS
        if self.stderr_bytes > STDERR_LIMIT:
            raise Refused("backend diagnostics budget")
        for byte in data:
            if byte == 10:
                # This acknowledgement follows interface/filter/output setup and header flush.
                if self.stderr_line == b"File: -":
                    self.backend_ready = True
                self.stderr_line.clear()
            else:
                self.stderr_line.append(byte)
                if len(self.stderr_line) > LINE_LIMIT:
                    raise Refused("backend diagnostic line budget")

    def admits_raw(self):
        return self.prefix.valid and self.prefix.count < self.frozen["packets"] and self.prefix.raw_bytes < self.frozen["maxBytes"]

    def stdout_interest(self):
        enabled = not self.stdout_eof and (not self.admits_raw() or self.raw_credit)
        try:
            self.selector.get_key(self.child.stdout)
            registered = True
        except KeyError:
            registered = False
        if enabled and not registered:
            self.selector.register(self.child.stdout, selectors.EVENT_READ, "stdout")
        elif registered and not enabled:
            self.selector.unregister(self.child.stdout)

    def stdout_ready(self):
        admits = self.admits_raw()
        target = min(CHUNK, self.prefix.needed, self.frozen["maxBytes"] - self.prefix.raw_bytes) if admits else CHUNK
        size = target - len(self.raw_buffer) if admits else CHUNK
        data = os.read(self.child.stdout.fileno(), size)
        if not data:
            self.stdout_eof = True
            self.selector.unregister(self.child.stdout)
            if self.raw_buffer:
                self.send_raw()
            return
        if self.pipe_until is not None:
            self.pipe_until = time.monotonic() + PIPE_SECONDS
        if not admits:
            self.discarded += len(data)
            if self.discarded > 16 * 1024 * 1024:
                raise Refused("backend ignored stop and drain budget")
            return
        self.raw_buffer.extend(data)
        # OS pipe fragments are not recording extents. Coalesce a bounded original unit (or
        # its EOF prefix) so small pipe reads cannot turn large captures into thousands of writes.
        if len(self.raw_buffer) == target:
            self.send_raw()

    def send_raw(self):
        data = bytes(self.raw_buffer)
        self.raw_buffer.clear()
        self.raw_sequence += 1
        self.send({"kind": "bytes", "sequence": self.raw_sequence, "data": base64.b64encode(data).decode("ascii")})
        self.pending_raw, self.raw_credit = data, False

    def publish_status(self):
        if not self.opened and self.backend_ready and self.prefix.valid and self.prefix.interface is not None and self.pending_raw is None:
            self.send({"kind": "ready"})
            self.opened, self.dirty = True, True
        phase = "stopping" if self.reason else "ready"
        now = time.monotonic()
        # A status is a coalesced observation, not one event per raw write. Raw credit and
        # acquisition remain independent of rolling-window processing or display speed.
        if self.opened and self.event_credit and self.dirty and (phase != self.last_phase or now >= self.next_status):
            self.event_sequence += 1
            self.send({"kind": "event", "sequence": self.event_sequence,
                       "data": self.prefix.status(self.frozen, phase)})
            self.event_credit, self.dirty = False, False
            self.last_phase, self.next_status = phase, now + STATUS_SECONDS

    def finish(self):
        if not self.opened:
            self.send({"kind": "failed"})
            return 2
        phase = "ended" if self.child.returncode == 0 and self.prefix.complete and not self.forced else "incomplete"
        reason = self.reason or ("ended" if self.child.returncode == 0 else "backend-failed")
        summary = self.prefix.status(self.frozen, phase)
        if self.control is not None:
            self.send({"kind": "control", "id": self.control, "outcome": {"kind": "confirmed", "data": {"stopped": True}}})
        if self.event_credit:
            self.event_sequence += 1
            self.send({"kind": "event", "sequence": self.event_sequence, "data": summary})
        self.send({"kind": "recording", "data": self.shell(phase, reason)})
        self.send({"kind": "end"})
        return 0

    def run(self):
        self.send({"kind": "recording", "data": self.shell("opening", "opening")})
        # The backend stays inside the host-owned process group. No traffic file or inherited secrets.
        self.child = subprocess.Popen(backend_argv(self.frozen), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, env={"LC_ALL": "C", "LANG": "C"}, close_fds=True)
        for pipe, role in ((self.input, "stdin"), (self.child.stderr, "stderr")):
            os.set_blocking(pipe.fileno(), False)
            self.selector.register(pipe, selectors.EVENT_READ, role)
        os.set_blocking(self.child.stdout.fileno(), False)
        started = time.monotonic()
        while not self.host_closed:
            now = time.monotonic()
            if now >= started + self.frozen["durationMs"] / 1000:
                self.stop("duration")
            if not self.opened and now >= started + 10:
                self.stop("startup-timeout")
            if self.stop_until is not None and now >= self.stop_until and self.child.poll() is None:
                self.child.kill()
                self.forced = True
            if self.child.poll() is not None and self.pipe_until is None:
                self.pipe_until = now + PIPE_SECONDS
            if self.child.poll() is not None and self.stdout_eof and self.stderr_eof and self.pending_raw is None:
                return self.finish()
            # Native append credit can take longer than a pipe EOF check. It remains governed by
            # the absolute recording lifetime; an accepted write never becomes an idle-pipe fault.
            if self.pipe_until is not None and self.pending_raw is None and now >= self.pipe_until and not (self.stdout_eof and self.stderr_eof):
                raise Refused("backend pipes did not physically close")
            self.stdout_interest()
            for key, _ in self.selector.select(0.05):
                {"stdin": self.stdin_ready, "stderr": self.stderr_ready, "stdout": self.stdout_ready}[key.data]()
                if self.host_closed:
                    break
            if not self.host_closed:
                self.publish_status()
        return 2

    def close(self):
        if self.child is not None:
            if self.child.poll() is None:
                self.child.send_signal(signal.SIGINT)
                try:
                    self.child.wait(timeout=STOP_SECONDS)
                except subprocess.TimeoutExpired:
                    self.child.kill()
            self.child.wait()
            self.child.stdout.close()
            self.child.stderr.close()
        self.selector.close()


def main():
    capture = None
    try:
        if sys.argv[1:] != ["start"]:
            raise Refused("operation")
        start = read_start(sys.stdin.buffer)
        frozen = start_profile(start)
        capture = Capture(start, frozen, sys.stdin.buffer, sys.stdout.buffer)
        return capture.run()
    except (Refused, OSError, ValueError, TypeError, KeyError, RecursionError, struct.error):
        # Never forward backend diagnostics, captured data, filesystem paths or tracebacks.
        if capture is not None and not capture.host_closed:
            try:
                if capture.control is not None:
                    capture.send({"kind": "control", "id": capture.control, "outcome": {"kind": "unknown"}})
                capture.send({"kind": "failed"})
            except OSError:
                pass
        return 2
    finally:
        if capture is not None:
            capture.close()


if __name__ == "__main__":
    sys.exit(main())
