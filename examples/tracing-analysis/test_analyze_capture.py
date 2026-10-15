"""Synthetic packet facts and streaming boundaries; no network or installed dissector."""
import io
import json
from pathlib import Path
import struct
import subprocess
import sys
import unittest
from unittest.mock import patch
import analyze_capture as analyzer

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tracing-offline"))
from test_capture_adapter import pcap, section, interface, enhanced, block


def identity():
    argv = json.loads(Path(analyzer.__file__).with_name("provider.yaml").read_text())["operations"][0]["argv"]
    return {"codeSha256": argv[2], "containerSha256": argv[4], "schemaSha256": argv[6]}


def ethernet(payload, kind=0x0800):
    return bytes.fromhex("020000000002020000000001") + struct.pack(">H", kind) + payload


def ipv4(payload, protocol=6, fragment=0, options=b"", total=None):
    size = 20 + len(options)
    return struct.pack(">BBHHHBBH4s4s", 0x40 | (size // 4), 0, size + len(payload) if total is None else total,
                       7, fragment, 64, protocol, 0, bytes([192, 0, 2, 10]), bytes([198, 51, 100, 20])) + options + payload


def tcp(options=b"", flags=0x12, header=None):
    size = 20 + len(options) if header is None else header
    return struct.pack(">HHIIBBHHH", 54321, 8080, 0xffffffff, 123, (size // 4) << 4, flags, 4096, 0, 0) + options


def udp(payload=b"", length=None):
    return struct.pack(">HHHH", 5353, 9000, 8 + len(payload) if length is None else length, 0) + payload


def analyze(data, packets=512, header_bytes=256, file=None):
    return json.loads(analyzer.analyze(file or io.BytesIO(data), len(data), packets, header_bytes, identity()))


class AnalysisTests(unittest.TestCase):
    def test_provider_pins_the_actual_decoder_container_and_single_type_schema(self):
        _source, actual = analyzer.identities()
        self.assertEqual(identity(), {key: actual[key] for key in identity()})
        self.assertEqual(actual["name"], "wes-packet-headers")

    def test_container_executes_only_the_frozen_verified_source_without_path_or_pyc_rereads(self):
        source, actual = analyzer.identities()
        with patch.object(analyzer, "identities", return_value=(source, actual)), \
             patch.object(analyzer, "bounded_file", side_effect=AssertionError("path reread")):
            container, identity_result = analyzer.checked_identity(identity())
        self.assertEqual(identity_result, actual)
        data = pcap(packets=[])
        self.assertEqual(container.index_file(io.BytesIO(data), len(data))["packetCount"], "0")

    def test_tcp_facts_and_each_field_range_refer_to_original_packet_bytes(self):
        payload = ethernet(ipv4(tcp()))
        data = pcap(packets=[payload])
        result = analyze(data)
        self.assertTrue(result["sampleComplete"])
        self.assertEqual(result["index"]["sha256"], analyzer.digest(data))
        packet = result["packets"][0]
        self.assertEqual((packet["protocol"], packet["status"]), ("TCP", "decoded"))
        self.assertEqual((packet["source"], packet["destination"], packet["sourcePort"], packet["destinationPort"]),
                         ("192.0.2.10", "198.51.100.20", 54321, 8080))
        fields = {(field["layer"], field["key"]): field for field in packet["fields"]}
        self.assertEqual(fields[("TCP", "sequence")]["value"], "4294967295")
        self.assertEqual(fields[("TCP", "flags")]["value"], "0x12 · SYN,ACK")
        port = fields[("TCP", "destinationPort")]
        self.assertEqual(data[int(port["offset"]):int(port["offset"]) + port["length"]], struct.pack(">H", 8080))
        for field in packet["fields"]:
            self.assertGreaterEqual(int(field["offset"]), int(packet["dataOffset"]))
            self.assertLessEqual(int(field["offset"]) + field["length"], int(packet["dataOffset"]) + packet["inspectedBytes"])
        self.assertIn("checksum validation", " ".join(result["limitations"]))

    def test_udp_header_lengths_and_unknown_payload_remain_separate(self):
        data = pcap(packets=[ethernet(ipv4(udp(b"uninterpreted payload"), protocol=17))])
        packet = analyze(data)["packets"][0]
        self.assertEqual((packet["protocol"], packet["status"], packet["sourcePort"]), ("UDP", "decoded", 5353))
        self.assertFalse(any("payload" in field["key"] for field in packet["fields"]))
        bad = ethernet(ipv4(udp(length=7), protocol=17))
        self.assertEqual(analyze(pcap(packets=[bad]))["packets"][0]["status"], "malformed")
        bad = ethernet(ipv4(udp(length=999), protocol=17))
        self.assertEqual(analyze(pcap(packets=[bad]))["packets"][0]["status"], "malformed")

    def test_ng_sections_packet_padding_and_unknown_blocks_do_not_change_ranges(self):
        one, two = ethernet(ipv4(tcp())), ethernet(ipv4(udp(b"z"), protocol=17))
        data = section() + interface() + block(0xabcd, b"skipped!") + enhanced(one)
        data += section(">") + interface(">") + enhanced(two, ">")
        result = analyze(data)
        self.assertEqual(result["index"]["skippedBlocks"], 1)
        self.assertEqual([p["interface"] for p in result["packets"]], [0, 1])
        for row, payload in zip(result["packets"], [one, two]):
            start = int(row["dataOffset"])
            self.assertEqual(data[start:start + row["capturedLength"]], payload)

    def test_fragments_never_infer_transport_ports_even_for_first_fragment(self):
        for bits in (0x2000, 1, 0x2001):
            packet = analyze(pcap(packets=[ethernet(ipv4(tcp(), fragment=bits))]))["packets"][0]
            self.assertEqual((packet["protocol"], packet["status"]), ("IPv4", "fragmented"))
            self.assertIsNone(packet["sourcePort"])
            self.assertFalse(any(field["layer"] == "TCP" for field in packet["fields"]))

    def test_unsupported_link_and_protocol_keep_exact_index_and_unknown_facts(self):
        data = pcap(packets=[ethernet(ipv4(tcp()))])
        data = data[:20] + struct.pack("<I", 101) + data[24:]
        packet = analyze(data)["packets"][0]
        self.assertEqual((packet["protocol"], packet["status"], packet["fields"]), ("unknown", "unsupported", []))
        self.assertIsNone(packet["source"])
        for payload in [ethernet(b"unknown", 0x86dd), ethernet(ipv4(b"unknown", protocol=1))]:
            packet = analyze(pcap(packets=[payload]))["packets"][0]
            self.assertEqual(packet["status"], "unsupported")
            self.assertIsNone(packet["destinationPort"])

    def test_truncated_packets_and_budget_exhaustion_are_not_claimed_as_full_decodes(self):
        for payload in (b"", b"short", ethernet(b"short"), ethernet(ipv4(tcp())[:-5])):
            packet = analyze(pcap(packets=[payload]))["packets"][0]
            self.assertEqual(packet["status"], "truncated")
        payload = ethernet(ipv4(tcp(b"\x01" * 40), options=b"\x01" * 40))
        packet = analyze(pcap(packets=[payload]), header_bytes=64)["packets"][0]
        self.assertEqual(packet["status"], "truncated")
        self.assertIn("inspected prefix", " ".join(packet["notes"]))
        self.assertEqual(packet["inspectedBytes"], 64)

    def test_invalid_ipv4_and_tcp_lengths_never_read_ethernet_padding_as_transport(self):
        for payload in (ethernet(ipv4(tcp(), total=10)), ethernet(ipv4(tcp(), total=20)),
                        ethernet(ipv4(tcp(header=16))), ethernet(ipv4(tcp(header=60)))):
            packet = analyze(pcap(packets=[payload]))["packets"][0]
            self.assertEqual(packet["status"], "malformed")

    def test_empty_sample_and_bounded_first_page_do_not_misreport_coverage(self):
        empty = analyze(pcap(packets=[]))
        self.assertTrue(empty["sampleComplete"])
        self.assertEqual(empty["packets"], [])
        data = pcap(packets=[ethernet(ipv4(tcp()))] * 513)
        result = analyze(data, packets=2)
        self.assertFalse(result["sampleComplete"])
        self.assertEqual((len(result["packets"]), len(result["index"]["packets"])), (2, 512))
        self.assertEqual(result["index"]["packetCount"], "513")
        self.assertEqual(result["index"]["sha256"], analyzer.digest(data))

    def test_no_seek_or_unbounded_read_or_payload_staging_and_originals_are_unchanged(self):
        class Sequential(io.BytesIO):
            def read(self, count=-1):
                if not 0 <= count <= 65536:
                    raise AssertionError("unbounded input read")
                return super().read(count)
            def seek(self, *_args):
                raise AssertionError("input seek")
        payload = ethernet(ipv4(udp(b"\0" * 1000), protocol=17))
        data = pcap(packets=[payload] * 8)
        result = analyze(data, packets=1, file=Sequential(data))
        self.assertEqual(result["index"]["rawBytes"], str(len(data)))
        self.assertEqual(result["index"]["sha256"], analyzer.digest(data))

    def test_identity_and_option_refusal_precedes_any_input_consumption(self):
        class Untouched:
            def read(self, _size):
                raise AssertionError("input consumed before preflight")
        expected = identity()
        expected["codeSha256"] = "sha256:" + "0" * 64
        with self.assertRaises(analyzer.AnalysisRefused):
            analyzer.analyze(Untouched(), 24, 1, 64, expected)
        for packets, header_bytes in ((0, 64), (513, 64), (1, 63), (1, 65537)):
            with self.assertRaises(analyzer.AnalysisRefused):
                analyzer.analyze(Untouched(), 24, packets, header_bytes, identity())

    def test_changed_extent_malformed_container_and_output_budget_refuse_without_result(self):
        data = pcap(packets=[ethernet(ipv4(tcp()))])
        for size in (len(data) - 1, len(data) + 1):
            with self.assertRaises(ValueError):
                analyzer.analyze(io.BytesIO(data), size, 1, 64, identity())
        with self.assertRaises(ValueError):
            analyze(b"unknown!")
        with patch.object(analyzer, "MAX_OUTPUT", 10), self.assertRaises(analyzer.AnalysisRefused):
            analyze(data)

    def test_real_cli_requires_native_extent_and_has_no_plaintext_error_payload(self):
        data = pcap(packets=[ethernet(ipv4(tcp()))])
        expected = identity()
        argv = [sys.executable, analyzer.__file__, "decode", "--packets", "1", "--header-bytes", "64",
                "--decoder-sha256", expected["codeSha256"], "--container-sha256", expected["containerSha256"],
                "--schema-sha256", expected["schemaSha256"]]
        result = subprocess.run(argv, input=data, capture_output=True, env={"WES_BYTE_INPUT_BYTES": str(len(data))})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["packets"][0]["destinationPort"], 8080)
        result = subprocess.run(argv, input=b"synthetic-invalid-payload", capture_output=True, env={})
        self.assertEqual((result.returncode, result.stdout), (2, b""))
        self.assertNotIn(b"synthetic-invalid-payload", result.stderr)
        self.assertNotIn(b"Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
