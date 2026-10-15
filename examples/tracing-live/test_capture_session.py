"""Independent capture acceptance with owned synthetic pipes, never real interfaces."""
import base64
import io
import json
import os
from pathlib import Path
import selectors
import signal
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import capture_session as adapter


class Peer:
    def __init__(self, mode="synthetic-hold", deadline_after=None, narrowing=None, **changes):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        backend = self.root / "synthetic-backend"
        fixture = Path(__file__).with_name("fixture_backend.py").read_text()
        backend.write_text("#!" + sys.executable + "\n" + fixture.split("\n", 1)[1])
        backend.chmod(0o700)
        self.profile = {"name": "synthetic capture", "backend": str(backend), "interface": mode, "filter": "tcp port 4242",
                        "durationMs": 10000, "maxBytes": 1048576, "packets": 1000, "snapLength": 65535, "promiscuous": False}
        self.profile.update(changes)
        self.session = "synthetic-attempt"
        start = {"version": 1, "session": self.session, "run": "synthetic-run", "operation": ["start"], "arguments": {"profile": self.profile},
                 "endpoint": None, "credentials": {}, "contracts": {"result": "status", "parameters": {"profile": "profile"}, "control": ["action", "receipt"]},
                 "event_credit": 1, "recording": {"contract": "raw", "byte_field": ["raw"], "max_bytes": 536870912, "max_duration_ms": 125000, "credit": 1}}
        if narrowing is not None:
            start["arguments"].update(narrowing)
            start["contracts"]["parameters"].update({name: "optional" for name in ("duration_ms", "packet_limit", "max_bytes", "filter_condition")})
        self.start = start
        helper = str(Path(adapter.__file__).resolve())
        command = [sys.executable, helper, "start"]
        if deadline_after is not None:
            # Freeze only the helper's clock until the real synthetic pipe proof, then advance
            # past the admitted deadline. Backend startup/cleanup and the driver's wait stay real.
            # This is a test wrapper around the actual helper, not a product clock/CLI port.
            wrapper = '''
import importlib.util,sys,time
spec=importlib.util.spec_from_file_location("capture_helper",sys.argv[1])
module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
proof=sys.argv[2]
class Clock:
    at=None
    offset=0
    def monotonic(self):
        return 0.0 if self.at is None else self.offset+time.monotonic()-self.at
    def advance(self,capture):
        if self.at is None:
            self.offset=capture.frozen["durationMs"]/1000+0.001
            self.at=time.monotonic()
clock=Clock(); module.time=clock
send=module.Capture.send
def observed_send(self,message):
    send(self,message)
    if proof=="ready" and message["kind"]=="ready": clock.advance(self)
module.Capture.send=observed_send
receive=module.Capture.receive
def observed_receive(self,frame):
    receive(self,frame)
    if proof=="header" and self.prefix.interface is not None: clock.advance(self)
module.Capture.receive=observed_receive
sys.argv=[spec.origin,"start"]
sys.exit(module.main())
'''
            command = [sys.executable, "-c", wrapper, helper, deadline_after]
        self.child = subprocess.Popen(command, cwd=self.root, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                                      env={**os.environ, "SYNTHETIC_SECRET": "must-not-reach-backend"})
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.child.stdout, selectors.EVENT_READ)
        os.set_blocking(self.child.stdout.fileno(), False)
        self.buffer, self.raw, self.messages = bytearray(), bytearray(), []
        self.raw_sequence, self.event_sequence, self.eof, self.hold_raw, self.hold_event = 0, 0, False, False, False
        self.write(start)

    def write(self, value):
        data = json.dumps(value, separators=(",", ":")).encode()
        self.child.stdin.write(struct.pack(">I", len(data)) + data)
        self.child.stdin.flush()

    def send(self, message):
        self.write({"version": 1, "session": self.session, "message": message})

    def pump(self, seconds=0.1):
        for key, _ in self.selector.select(seconds):
            data = os.read(key.fd, 65536)
            if not data:
                self.eof = True
                self.selector.unregister(self.child.stdout)
                return
            self.buffer.extend(data)
        while len(self.buffer) >= 4:
            size = struct.unpack(">I", self.buffer[:4])[0]
            assert 1 <= size <= adapter.FRAME_LIMIT
            if len(self.buffer) < size + 4:
                return
            frame = json.loads(self.buffer[4:4 + size])
            del self.buffer[:4 + size]
            assert set(frame) == {"version", "session", "message"} and frame["version"] == 1 and frame["session"] == self.session
            msg = frame["message"]
            self.messages.append(msg)
            if msg["kind"] == "bytes":
                self.raw_sequence += 1
                assert msg["sequence"] == self.raw_sequence
                data = base64.b64decode(msg["data"], validate=True)
                assert 1 <= len(data) <= 65536 and base64.b64encode(data).decode() == msg["data"]
                self.raw.extend(data)
                if not self.hold_raw:
                    self.send({"kind": "bytes-credit", "count": 1})
            elif msg["kind"] == "event":
                self.event_sequence += 1
                assert msg["sequence"] == self.event_sequence
                assert any(m["kind"] == "ready" for m in self.messages)
                if not self.hold_event:
                    self.send({"kind": "credit", "count": 1})

    def until(self, kind, seconds=8):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            matches = [m for m in self.messages if m["kind"] == kind]
            if matches:
                return matches[-1]
            if self.eof:
                raise AssertionError(f"EOF before {kind}: {self.messages[-3:]}")
            self.pump()
        raise AssertionError(f"No {kind}: {self.messages[-3:]}")

    def stop(self):
        self.send({"kind": "control", "id": "synthetic-stop", "data": {"operation": "stop"}})
        receipt = self.until("control")
        self.until("end")
        assert receipt["id"] == "synthetic-stop"
        assert receipt["outcome"]["kind"] == "confirmed"
        assert receipt["outcome"]["data"] == {"stopped": True}
        return self.final()

    def final(self):
        shells = [m["data"] for m in self.messages if m["kind"] == "recording"]
        assert len(shells) == 2 and shells[0]["summary"]["phase"] == "opening"
        assert shells[1]["raw"] == "" and shells[1]["profile"] == self.profile
        assert shells[1]["summary"]["rawBytes"] == str(len(self.raw))
        assert shells[1]["summary"]["coverageKnown"] is False and shells[1]["summary"]["drops"] is None
        return shells[1]

    def close(self):
        if not self.child.stdin.closed:
            self.child.stdin.close()
        try:
            self.child.wait(timeout=4)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(self.child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.child.wait(timeout=2)
        self.child.stdout.close()
        self.child.stderr.close()
        self.selector.close()
        self.temp.cleanup()


@unittest.skipUnless(os.name == "posix", "Local capture helper currently owns Unix pipes")
class CaptureTests(unittest.TestCase):
    def peer(self, *args, **kwargs):
        peer = Peer(*args, **kwargs)
        self.addCleanup(peer.close)
        return peer

    def test_original_raw_index_and_explicit_argv_without_inherited_secrets(self):
        peer = self.peer("synthetic-normal")
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["summary"]["phase"], "ended")
        self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))
        argv = json.loads((peer.root / "backend.arguments.json").read_text())
        self.assertEqual(argv, adapter.backend_argv(peer.profile)[1:])
        self.assertNotIn("-Q", argv)
        self.assertEqual((peer.root / "attempts").read_text(), "1\n")
        self.assertEqual(bytes(peer.raw[40:]), __import__("fixture_backend").PAYLOAD)

    def test_options_only_narrow_and_report_the_effective_profile_without_mutating_the_base(self):
        peer = self.peer("synthetic-idle", narrowing={
            "duration_ms": 8000, "packet_limit": 12, "max_bytes": 4096, "filter_condition": "dst host 127.0.0.1"})
        peer.until("ready")
        original = dict(peer.profile)
        expected = {**original, "durationMs": 8000, "packets": 12, "maxBytes": 4096,
                    "filter": "(tcp port 4242 and dst host 127.0.0.1)"}
        peer.profile = expected
        final = peer.stop()
        self.assertEqual(final["profile"], expected)
        self.assertEqual(final["summary"]["profile"], expected)
        self.assertEqual(peer.start["arguments"]["profile"], original)
        self.assertEqual(adapter.start_profile(peer.start), expected)
        self.assertEqual(peer.start["arguments"]["profile"], original)
        self.assertEqual(len(peer.raw), 24)
        self.assertEqual((peer.root / "attempts").read_text(), "1\n")
        self.assertEqual(json.loads((peer.root / "backend.arguments.json").read_text()),
                         ["-i", "synthetic-idle", "-f", expected["filter"], "-F", "pcap", "-q",
                          "-s", "65535", "-c", "12", "-a", "duration:8.000", "-w", "-", "-p"])

    def test_narrowed_byte_ceiling_keeps_only_the_actual_original_prefix(self):
        peer = self.peer("synthetic-normal", narrowing={"max_bytes": 43})
        peer.profile = {**peer.profile, "maxBytes": 43}
        peer.until("end")
        final = peer.final()
        self.assertEqual((len(peer.raw), final["reason"]), (43, "bytes"))
        self.assertFalse(final["summary"]["containerComplete"])
        self.assertIsNone(final["index"])
        self.assertEqual(bytes(peer.raw[40:]), __import__("fixture_backend").PAYLOAD[:3])
        self.assertEqual(peer.start["arguments"]["profile"]["maxBytes"], 1048576)

    def test_unsupported_base_filter_can_only_receive_numeric_narrowing(self):
        base = "vlan 7 and tcp[13] & 2 != 0"
        peer = self.peer("synthetic-idle", filter=base, narrowing={"packet_limit": 1})
        peer.profile = {**peer.profile, "packets": 1}
        peer.until("ready")
        self.assertEqual(peer.stop()["profile"]["filter"], base)
        rejected = self.peer(filter=base, narrowing={"filter_condition": "port 4242"})
        self.assertEqual(rejected.child.wait(timeout=2), 2)
        self.assertFalse((rejected.root / "attempts").exists())

    def test_combined_filter_output_budget_refuses_before_backend_entry(self):
        expression = " or ".join(["host ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"] * 70)
        self.assertLess(len(adapter.Predicate(expression).render()), 4096)
        peer = self.peer(filter=expression, narrowing={"filter_condition": expression})
        self.assertEqual(peer.child.wait(timeout=2), 2)
        self.assertFalse((peer.root / "attempts").exists())

    def test_widening_and_unsafe_conditions_refuse_before_backend_entry(self):
        options = [
            {"duration_ms": 10001}, {"duration_ms": 0}, {"duration_ms": -1}, {"duration_ms": True},
            {"duration_ms": "8000"}, {"duration_ms": None}, {"packet_limit": 1001}, {"packet_limit": 0},
            {"max_bytes": 1048577}, {"max_bytes": 23}, {"max_bytes": False}, {"interface": "other"},
            {"filter_condition": "tcp) or (udp"}, {"filter_condition": "host synthetic.invalid"},
            {"filter_condition": "tcp or vlan 7"}, {"filter_condition": "tcp[13] & 2 != 0"},
            {"filter_condition": "port 4242 # comment"}, {"filter_condition": "port 4242\nor udp"},
            {"filter_condition": "port 4242; echo unsafe"}, {"filter_condition": "'tcp'"},
            {"filter_condition": "tcp\\or udp"}, {"filter_condition": "vlan 1 and tcp"},
            {"filter_condition": ""}, {"filter_condition": "tcp " * 1025},
        ]
        for narrowing in options:
            with self.subTest(narrowing=narrowing):
                peer = self.peer(narrowing=narrowing)
                self.assertEqual(peer.child.wait(timeout=2), 2)
                self.assertFalse((peer.root / "attempts").exists())

    def test_startup_banner_never_counts_as_ready(self):
        peer = self.peer("synthetic-slow-start")
        until = time.monotonic() + 5
        while not (peer.root / "backend.banner").exists() and time.monotonic() < until:
            peer.pump(0.01)
        self.assertTrue((peer.root / "backend.banner").exists())
        peer.pump(0.1)
        self.assertFalse(any(m["kind"] == "ready" for m in peer.messages))
        (peer.root / "allow-ready").write_text("backend setup completed")
        peer.until("ready")
        final = peer.stop()
        self.assertEqual(final["reason"], "stopped")
        self.assertEqual(final["summary"]["phase"], "ended")

    def test_ready_waits_for_native_header_ack_and_stop_remains_responsive(self):
        peer = self.peer("synthetic-idle")
        peer.hold_raw = True
        peer.until("bytes")
        self.assertEqual(len(peer.raw), 24)
        peer.pump(0.15)
        self.assertFalse(any(m["kind"] == "ready" for m in peer.messages))
        peer.hold_raw = False
        peer.send({"kind": "bytes-credit", "count": 1})
        peer.until("ready")
        final = peer.stop()
        self.assertEqual(final["summary"]["packetCount"], "0")
        self.assertTrue(final["summary"]["containerComplete"])
        self.assertEqual(final["index"]["packets"], [])

    def test_idle_capture_duration_and_unknown_loss_are_real_facts(self):
        peer = self.peer("synthetic-idle", durationMs=250, deadline_after="ready")
        peer.until("ready")
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["reason"], "duration")
        self.assertEqual(final["summary"]["packetCount"], "0")
        self.assertEqual(len(peer.raw), 24)

    def test_stop_reaches_backend_while_an_admitted_raw_append_is_waiting(self):
        peer = self.peer("synthetic-held-packet")
        peer.until("ready")
        peer.hold_raw = True
        (peer.root / "allow-packet").write_text("packet may arrive")
        until = time.monotonic() + 5
        while peer.raw_sequence < 2 and time.monotonic() < until:
            peer.pump(0.01)
        self.assertEqual(peer.raw_sequence, 2)
        peer.send({"kind": "control", "id": "synthetic-stop", "data": {"operation": "stop"}})
        until = time.monotonic() + 1
        while not (peer.root / "backend.stopped").exists() and time.monotonic() < until:
            peer.pump(0.01)
        self.assertTrue((peer.root / "backend.stopped").exists())
        self.assertFalse(any(m["kind"] == "control" for m in peer.messages))
        peer.hold_raw = False
        peer.send({"kind": "bytes-credit", "count": 1})
        self.assertEqual(peer.until("control")["outcome"]["kind"], "confirmed")
        peer.until("end")
        self.assertEqual(peer.final()["summary"]["packetCount"], "1")

    def test_byte_limit_preserves_exact_partial_container_without_fake_index(self):
        peer = self.peer("synthetic-normal", maxBytes=43)
        peer.until("end")
        final = peer.final()
        self.assertEqual((len(peer.raw), final["reason"]), (43, "bytes"))
        self.assertEqual(final["summary"]["phase"], "incomplete")
        self.assertFalse(final["summary"]["containerComplete"])
        self.assertIsNone(final["index"])
        self.assertEqual(bytes(peer.raw[40:]), __import__("fixture_backend").PAYLOAD[:3])

    def test_packet_limit_stops_at_original_record_boundary(self):
        peer = self.peer("synthetic-index-window", packets=1)
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["reason"], "packets")
        self.assertEqual(final["summary"]["packetCount"], "1")
        self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))

    def test_513_packets_keep_original_bytes_and_bound_only_the_index(self):
        peer = self.peer("synthetic-index-window")
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["summary"]["packetCount"], "513")
        self.assertEqual(len(final["index"]["packets"]), 512)
        self.assertFalse(final["summary"]["indexComplete"])
        self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))
        # The status window observes the capture; it must not accumulate one event per packet/write.
        self.assertLess(len([m for m in peer.messages if m["kind"] == "event"]), 513)

    def test_malformed_and_truncated_packets_leave_an_explicit_original_prefix(self):
        for mode in ("synthetic-malformed-packet", "synthetic-partial-packet"):
            with self.subTest(mode=mode):
                peer = self.peer(mode)
                peer.until("end")
                final = peer.final()
                self.assertEqual(final["summary"]["phase"], "incomplete")
                self.assertEqual(final["summary"]["packetCount"], "0")
                self.assertFalse(final["summary"]["containerComplete"])
                self.assertIsNone(final["index"])
                self.assertGreaterEqual(len(peer.raw), 40)

    def test_window_backpressure_does_not_replace_or_block_the_raw_recording(self):
        peer = self.peer("synthetic-index-window")
        peer.hold_event = True
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["summary"]["packetCount"], "513")
        self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))
        self.assertEqual(len([m for m in peer.messages if m["kind"] == "event"]), 1)

    def test_backend_failure_preserves_valid_container_bytes_with_incomplete_coverage(self):
        peer = self.peer("synthetic-failed")
        peer.until("end")
        final = peer.final()
        self.assertEqual(final["reason"], "backend-failed")
        self.assertEqual(final["summary"]["phase"], "incomplete")
        self.assertTrue(final["summary"]["containerComplete"])
        self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))
        self.assertEqual((peer.root / "attempts").read_text(), "1\n")

    def test_byte_order_resolution_and_truncation_are_container_facts(self):
        for mode in ("synthetic-big-endian", "synthetic-truncated"):
            with self.subTest(mode=mode):
                peer = self.peer(mode)
                peer.until("end")
                final = peer.final()
                self.assertEqual(final["index"], adapter.container.index_file(io.BytesIO(peer.raw), len(peer.raw)))
                self.assertEqual(final["summary"]["truncatedPackets"], "1" if mode.endswith("truncated") else "0")

    def test_pre_ready_invalid_header_missing_ack_and_diagnostics_never_open(self):
        for mode in ("synthetic-invalid-header", "synthetic-no-ack", "synthetic-diagnostics"):
            with self.subTest(mode=mode):
                peer = self.peer(mode, deadline_after="header" if mode == "synthetic-no-ack" else None)
                peer.until("failed")
                self.assertFalse(any(m["kind"] == "ready" for m in peer.messages))
                self.assertEqual((peer.root / "attempts").read_text(), "1\n")

    def test_host_eof_joins_the_backend_without_restart(self):
        peer = self.peer("synthetic-idle")
        peer.until("ready")
        pid = int((peer.root / "backend.pid").read_text())
        peer.child.stdin.close()
        self.assertEqual(peer.child.wait(timeout=4), 2)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertEqual((peer.root / "attempts").read_text(), "1\n")

    def test_forced_stop_confirms_join_but_never_successful_capture(self):
        peer = self.peer("synthetic-ignore-stop")
        peer.until("ready")
        final = peer.stop()
        self.assertEqual(final["summary"]["phase"], "incomplete")
        self.assertEqual(final["reason"], "stopped")
        self.assertEqual((peer.root / "attempts").read_text(), "1\n")

    def test_invalid_profiles_refuse_before_any_backend_launch(self):
        for changes in ({"backend": "relative"}, {"interface": "rpcap://synthetic"}, {"interface": "TCP@synthetic"}, {"interface": "-"},
                        {"durationMs": True}, {"maxBytes": 23}, {"snapLength": 65536}, {"filter": ""}, {"name": "synthetic\x00"}):
            with self.subTest(changes=changes):
                peer = self.peer(**changes)
                self.assertEqual(peer.child.wait(timeout=2), 2)
                self.assertFalse((peer.root / "attempts").exists())


class PredicateTests(unittest.TestCase):
    def test_explicit_predicates_and_libpcap_left_association_are_preserved(self):
        for source, expected in [
            ("tcp or udp and host 127.0.0.1", "((tcp or udp) and host 127.0.0.1)"),
            ("tcp or (udp and host 127.0.0.1)", "(tcp or (udp and host 127.0.0.1))"),
            ("!(tcp || udp) && src net 192.0.2.0/24", "((not (tcp or udp)) and src net 192.0.2.0/24)"),
            ("tcp src portrange 0-65535", "tcp src portrange 0-65535"),
            ("dst host 2001:db8::1", "dst host 2001:db8::1"),
            ("net 2001:db8::/32", "net 2001:db8::/32"),
            ("ip6 or ip", "(ip6 or ip)"),
        ]:
            with self.subTest(source=source):
                self.assertEqual(adapter.Predicate(source).render(), expected)

    def test_unsupported_ambiguous_and_work_limited_filters_refuse(self):
        for source in [
            "", " ", "TCP", "tcp udp", "tcp host 127.0.0.1", "ip port 80", "tcp src host 127.0.0.1",
            "src tcp port 80", "port 080", "port 65536", "portrange 90-80", "portrange 1-2-3",
            "host localhost", "host 127.1", "host fe80::1%lo", "net 192.0.2.1/24", "net 192.0.2.0",
            "net 192.0.2.0/255.255.255.0", "net 192.0.2.0/024", "port 80 or 443", "tcp)", "(tcp", "()", "not",
            "tcp and", "tcp or mpls 7", "pppoes and tcp", "geneve and tcp", "ether host 00:00:00:00:00:00",
            "tcp # comment", "tcp\nor udp", "tcp “or” udp", "tcp\\or udp", "tcp;udp", "tcp[0] = 1",
            "(" * 33 + "tcp" + ")" * 33, "not " * 33 + "tcp", "tcp or " * 129 + "udp", " " * 4097,
        ]:
            with self.subTest(source=source):
                with self.assertRaises(adapter.Refused):
                    adapter.Predicate(source).render()


if __name__ == "__main__":
    unittest.main()
