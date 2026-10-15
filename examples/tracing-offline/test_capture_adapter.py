"""Synthetic container fixtures; no packet capture tools, interfaces or services."""
import io
import json
import os
import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import capture_adapter as adapter


def pcap(endian="<", nano=False, packets=None):
    magic = (b"\x4d\x3c\xb2\xa1" if nano else b"\xd4\xc3\xb2\xa1") if endian == "<" else (b"\xa1\xb2\x3c\x4d" if nano else b"\xa1\xb2\xc3\xd4")
    body = magic + struct.pack(endian + "HHiIII", 2, 4, 0, 0, 65535, 1)
    for payload in packets if packets is not None else [b"\x00\xff\x7f"]:
        body += struct.pack(endian + "IIII", 17, 11, len(payload), len(payload)) + payload
    return body


def block(kind, body, endian="<"):
    return struct.pack(endian + "II", kind, len(body) + 12) + body + struct.pack(endian + "I", len(body) + 12)


def section(endian="<", declared=-1):
    return block(0x0a0d0d0a, struct.pack(endian + "IHHq", 0x1a2b3c4d, 1, 0, declared), endian)


def interface(endian="<", snap=65535, resolution=6, offset=0):
    options = struct.pack(endian + "HH", 9, 1) + bytes([resolution, 0, 0, 0])
    options += struct.pack(endian + "HHqHH", 14, 8, offset, 0, 0)
    return block(1, struct.pack(endian + "HHI", 1, 0, snap) + options, endian)


def enhanced(payload=b"abc", endian="<", iface=0, ticks=(1 << 53) + 7):
    return block(6, struct.pack(endian + "IIIII", iface, ticks >> 32, ticks & 0xffffffff, len(payload), len(payload)) + payload + b"\0" * (-len(payload) % 4), endian)


def index(data):
    return adapter.index_file(io.BytesIO(data), len(data))


class CaptureTests(unittest.TestCase):
    def test_classic_byte_orders_resolutions_and_original_coordinates(self):
        for endian in ("<", ">"):
            for nano in (False, True):
                data = pcap(endian, nano)
                result = index(data)
                packet = result["packets"][0]
                self.assertEqual(data[int(packet["dataOffset"]):][:packet["capturedLength"]], b"\x00\xff\x7f")
                self.assertEqual(packet["recordOffset"], "24")
                self.assertEqual(packet["dataOffset"], "40")
                self.assertEqual(packet["timestampTicks"], str(17 * 10 ** (9 if nano else 6) + 11))
                self.assertTrue(result["indexComplete"])

    def test_empty_packet_lists_and_truncated_captured_packets(self):
        self.assertEqual(index(pcap(packets=[]))["packetCount"], "0")
        self.assertEqual(index(section())["interfaces"], [])
        data = pcap()
        data = data[:36] + struct.pack("<I", 99) + data[40:]
        self.assertEqual(index(data)["packets"][0]["originalLength"], 99)

    def test_mixed_sections_interfaces_exact_timestamps_and_simple_packet_unknown_time(self):
        first = section() + interface(resolution=128 | 10, offset=-3) + enhanced()
        second = section(">") + interface(">", snap=2) + block(3, struct.pack(">I", 7) + b"xy\0\0", ">")
        result = index(first + second)
        self.assertEqual(result["interfaces"][0]["timestampScale"], "2^10")
        self.assertEqual(result["interfaces"][0]["timestampOffset"], "-3")
        self.assertEqual(result["packets"][0]["timestampTicks"], str((1 << 53) + 7))
        self.assertEqual(result["packets"][1]["interface"], 1)
        self.assertIsNone(result["packets"][1]["timestampTicks"])
        p = result["packets"][1]
        self.assertEqual((first + second)[int(p["dataOffset"]):int(p["dataOffset"]) + p["capturedLength"]], b"xy")

    def test_first_page_is_bounded_and_explicitly_incomplete(self):
        result = index(pcap(packets=[b"x"] * (adapter.INDEX_PACKETS + 1)))
        self.assertEqual(len(result["packets"]), adapter.INDEX_PACKETS)
        self.assertEqual(result["packetCount"], str(adapter.INDEX_PACKETS + 1))
        self.assertFalse(result["indexComplete"])

    def test_unknown_blocks_preserve_raw_but_obsolete_packet_blocks_refuse(self):
        result = index(section() + block(0xabcd, b"synthetic\0\0\0"))
        self.assertEqual(result["skippedBlocks"], 1)
        with self.assertRaises(adapter.InvalidCapture):
            index(section() + block(2, b""))

    def test_declared_section_length_and_trailers_are_exact(self):
        body = interface() + enhanced()
        self.assertEqual(index(section(declared=len(body)) + body)["packetCount"], "1")
        for declared in (4, len(body) - 4, len(body) + 4, -2, 1):
            with self.assertRaises(adapter.InvalidCapture):
                index(section(declared=declared) + body)
        data = section() + interface() + enhanced()
        with self.assertRaises(adapter.InvalidCapture):
            index(data[:-4] + struct.pack("<I", 12))

    def test_malformed_classic_and_ng_inputs_fail_without_unbounded_reads(self):
        good = pcap()
        malformed = [b"", b"abcd", good[:-1], good[:30], good + b"x", section() + enhanced(), section() + interface() + enhanced(iface=1)]
        malformed += [section() + block(3, struct.pack("<I", 7) + b"x\0\0\0")]
        malformed += [good[:24] + struct.pack("<IIII", 1, 1000000, 3, 3) + b"abc"]
        malformed += [good[:24] + struct.pack("<IIII", 1, 0, 4, 3) + b"abcd"]
        for data in malformed:
            with self.subTest(size=len(data)), self.assertRaises(adapter.InvalidCapture):
                index(data)

    def test_timestamp_options_reject_duplicate_and_wrong_lengths(self):
        for options in (struct.pack("<HHI", 9, 4, 6), struct.pack("<HH", 9, 1) + b"\x06\0\0\0" + struct.pack("<HH", 9, 1) + b"\x06\0\0\0"):
            with self.assertRaises(adapter.InvalidCapture):
                index(section() + block(1, struct.pack("<HHI", 1, 0, 0) + options))

    def test_file_interface_and_record_limits_are_enforced(self):
        with patch.object(adapter, "MAX_FILE", 10), self.assertRaises(adapter.InvalidCapture):
            index(pcap())
        with patch.object(adapter, "MAX_INTERFACES", 1), self.assertRaises(adapter.InvalidCapture):
            index(section() + interface() * 2)
        with patch.object(adapter, "MAX_RECORDS", 1), self.assertRaises(adapter.InvalidCapture):
            index(pcap(packets=[b"x", b"y"]))

    def test_import_framing_payload_and_hash_identity(self):
        with tempfile.TemporaryDirectory() as root:
            data = section() + interface() + enhanced(b"binary\0\xff")
            file = Path(root) / "synthetic.pcapng"
            file.write_bytes(data)
            out = io.BytesIO()
            adapter.import_file(file, out)
            encoded = out.getvalue()
            length = struct.unpack(">I", encoded[:4])[0]
            header = json.loads(encoded[4:4 + length])
            self.assertEqual(header["raw"], "")
            self.assertEqual(encoded[4 + length:], data)
            self.assertEqual(header["index"], index(data))

    def test_changed_input_refuses_even_after_header_was_emitted(self):
        with tempfile.TemporaryDirectory() as root:
            file = Path(root) / "synthetic.pcap"
            data = pcap()
            file.write_bytes(data)
            class ChangingOutput(io.BytesIO):
                def write(self, value):
                    result = super().write(value)
                    file.write_bytes(data[:-1] + b"!")
                    return result
            with self.assertRaises(adapter.InvalidCapture):
                adapter.import_file(file, ChangingOutput())

    def test_special_files_and_final_symlinks_are_refused(self):
        with tempfile.TemporaryDirectory() as root:
            folder = Path(root)
            with self.assertRaises((adapter.InvalidCapture, OSError)):
                adapter.import_file(folder, io.BytesIO())
            if hasattr(os, "mkfifo"):
                fifo = folder / "fifo"
                os.mkfifo(fifo)
                with self.assertRaises(adapter.InvalidCapture):
                    adapter.import_file(fifo, io.BytesIO())
            if hasattr(os, "O_NOFOLLOW"):
                file = folder / "synthetic"
                file.write_bytes(pcap())
                link = folder / "link"
                link.symlink_to(file)
                with self.assertRaises(OSError):
                    adapter.import_file(link, io.BytesIO())


if __name__ == "__main__":
    unittest.main()
