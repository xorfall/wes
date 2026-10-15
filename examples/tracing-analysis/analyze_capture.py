#!/usr/bin/env python3
"""Finite offline header analysis over exact owned stdin; no network, capture or staging."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import struct
import sys
import types

MAX_OUTPUT = 8 * 1024 * 1024
IDENTITY_LIMIT = 1024 * 1024
LIMITATIONS = [
    "Fixed Ethernet/IPv4/TCP/UDP headers only; other link types and protocols are unsupported.",
    "Packet sample and header prefix are bounded by the recorded options; filters cover this sample only.",
    "No payload dissection, options interpretation, checksum validation, fragment or stream reassembly.",
    "No connection, retransmission, RTT, decryption, drop or network traffic coverage claims.",
]


class AnalysisRefused(ValueError):
    pass


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def bounded_file(path):
    with path.open("rb") as file:
        data = file.read(IDENTITY_LIMIT + 1)
    if len(data) > IDENTITY_LIMIT:
        raise AnalysisRefused("decoder identity budget exceeded")
    return data


def distinct_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise AnalysisRefused("duplicate schema key")
        result[key] = value
    return result


def identities():
    root = Path(__file__).resolve().parent
    container = root.parent / "tracing-offline" / "capture_adapter.py"
    provider = json.loads(bounded_file(root / "provider.yaml"), object_pairs_hook=distinct_object)
    package = json.loads(bounded_file(root / "types.yaml"), object_pairs_hook=distinct_object)
    if package.get("version") != 1 or package.get("types") != provider["types"]:
        raise AnalysisRefused("workspace and provider type declarations differ")
    schema = json.dumps(provider["types"], sort_keys=True, separators=(",", ":")).encode()
    container_source = bounded_file(container)
    return container_source, {
        "name": "wes-packet-headers", "version": "1", "runtime": "Python " + platform.python_version(),
        "codeSha256": digest(bounded_file(Path(__file__))),
        "containerSha256": digest(container_source), "schemaSha256": digest(schema),
    }


def checked_identity(expected):
    source, actual = identities()
    for key in ("codeSha256", "containerSha256", "schemaSha256"):
        if expected.get(key) != actual[key]:
            raise AnalysisRefused("decoder, container or schema identity changed")
    # Execute exactly the bytes just checked, without a second path read or a pyc cache.
    path = Path(__file__).resolve().parent.parent / "tracing-offline" / "capture_adapter.py"
    module = types.ModuleType("wes_offline_container")
    module.__file__ = str(path)
    exec(compile(source, str(path), "exec"), module.__dict__)
    return module, actual


def decode(entry, interface, data):
    result = {key: entry[key] for key in ("number", "interface", "dataOffset", "capturedLength", "originalLength")}
    result.update(protocol="unknown", source=None, destination=None, sourcePort=None, destinationPort=None,
                  status="decoded", inspectedBytes=len(data), fields=[], notes=[])
    base = int(entry["dataOffset"])
    priority = {"decoded": 0, "unsupported": 1, "truncated": 2, "fragmented": 3, "malformed": 4}

    def issue(status, note):
        if priority[status] > priority[result["status"]]:
            result["status"] = status
        result["notes"].append(note)

    def require(offset, length, layer):
        if offset + length <= len(data):
            return True
        issue("truncated", layer + (" header exceeds inspected prefix." if offset + length <= entry["capturedLength"]
                                   else " header is incomplete in captured bytes."))
        return False

    def field(layer, key, label, offset, length, value):
        if offset < 0 or length < 1 or offset + length > len(data):
            raise AnalysisRefused("decoder produced a range outside inspected bytes")
        result["fields"].append({"layer": layer, "key": key, "label": label, "value": str(value),
                                 "offset": str(base + offset), "length": length})

    def u16(offset):
        return struct.unpack_from(">H", data, offset)[0]

    def u32(offset):
        return struct.unpack_from(">I", data, offset)[0]

    if entry["capturedLength"] < entry["originalLength"]:
        issue("truncated", "Captured packet is shorter than its recorded original length.")
    if interface["linkType"] != 1:
        issue("unsupported", "Unsupported link type " + str(interface["linkType"]) + ".")
        return result
    result["protocol"] = "Ethernet"
    if not require(0, 14, "Ethernet"):
        return result
    mac = lambda offset: ":".join(f"{byte:02x}" for byte in data[offset:offset + 6])
    field("Ethernet", "destination", "Destination MAC", 0, 6, mac(0))
    field("Ethernet", "source", "Source MAC", 6, 6, mac(6))
    ethertype = u16(12)
    field("Ethernet", "type", "EtherType", 12, 2, f"0x{ethertype:04x}")
    if ethertype != 0x0800:
        issue("unsupported", "EtherType is not supported by this IPv4 decoder.")
        return result
    result["protocol"] = "IPv4"
    ip = 14
    if not require(ip, 20, "IPv4"):
        return result
    version, ihl = data[ip] >> 4, (data[ip] & 15) * 4
    field("IPv4", "version", "Version", ip, 1, version)
    field("IPv4", "headerLength", "Header length in bytes", ip, 1, ihl)
    if version != 4 or ihl < 20:
        issue("malformed", "Invalid IPv4 version or header length.")
        return result
    total, fragment, protocol = u16(ip + 2), u16(ip + 6), data[ip + 9]
    field("IPv4", "service", "Service octet", ip + 1, 1, f"0x{data[ip + 1]:02x}")
    field("IPv4", "totalLength", "Total length", ip + 2, 2, total)
    field("IPv4", "id", "Identification", ip + 4, 2, u16(ip + 4))
    field("IPv4", "flags", "Flags (3 bits)", ip + 6, 2, fragment >> 13)
    field("IPv4", "fragmentOffset", "Fragment offset (8-byte units)", ip + 6, 2, fragment & 0x1fff)
    field("IPv4", "ttl", "Time to live", ip + 8, 1, data[ip + 8])
    field("IPv4", "protocol", "Protocol number", ip + 9, 1, protocol)
    field("IPv4", "checksum", "Header checksum (unverified)", ip + 10, 2, f"0x{u16(ip + 10):04x}")
    for name, offset in (("source", ip + 12), ("destination", ip + 16)):
        result[name] = ".".join(str(byte) for byte in data[offset:offset + 4])
        field("IPv4", name, name.capitalize() + " address", offset, 4, result[name])
    if total < ihl:
        issue("malformed", "IPv4 total length is smaller than its header.")
        return result
    if total + ip > entry["capturedLength"]:
        issue("truncated", "IPv4 declared data extends beyond captured bytes.")
    if not require(ip, ihl, "IPv4"):
        return result
    if ihl > 20:
        result["notes"].append("IPv4 options are present and uninterpreted.")
    if fragment & 0x3fff:
        issue("fragmented", "IPv4 fragment; transport headers are not decoded without reassembly.")
        return result
    if protocol not in (6, 17):
        issue("unsupported", "IPv4 protocol number is unsupported.")
        return result
    layer = "TCP" if protocol == 6 else "UDP"
    result["protocol"] = layer
    transport, size = ip + ihl, 20 if protocol == 6 else 8
    if total - ihl < size:
        issue("malformed", layer + " minimum header does not fit the IPv4 declared length.")
        return result
    if not require(transport, size, layer):
        return result
    for name, offset in (("sourcePort", transport), ("destinationPort", transport + 2)):
        result[name] = u16(offset)
        field(layer, name, "Source port" if name == "sourcePort" else "Destination port", offset, 2, result[name])
    if protocol == 17:
        length = u16(transport + 4)
        field(layer, "length", "Datagram length", transport + 4, 2, length)
        field(layer, "checksum", "Checksum (unverified)", transport + 6, 2, f"0x{u16(transport + 6):04x}")
        if length < 8 or length > total - ihl:
            issue("malformed", "UDP length is invalid for its IPv4 datagram.")
    else:
        field(layer, "sequence", "Sequence number (raw)", transport + 4, 4, u32(transport + 4))
        field(layer, "acknowledgment", "Acknowledgment field (raw)", transport + 8, 4, u32(transport + 8))
        header = (data[transport + 12] >> 4) * 4
        field(layer, "headerLength", "Header length in bytes", transport + 12, 1, header)
        flags = data[transport + 13]
        names = ("FIN", "SYN", "RST", "PSH", "ACK", "URG", "ECE", "CWR")
        field(layer, "flags", "Flags", transport + 13, 1,
              f"0x{flags:02x}" + (" · " + ",".join(name for bit, name in enumerate(names) if flags & (1 << bit)) if flags else ""))
        field(layer, "window", "Window field (raw)", transport + 14, 2, u16(transport + 14))
        field(layer, "checksum", "Checksum (unverified)", transport + 16, 2, f"0x{u16(transport + 16):04x}")
        field(layer, "urgent", "Urgent pointer field (raw)", transport + 18, 2, u16(transport + 18))
        if header < 20 or header > total - ihl:
            issue("malformed", "TCP header length is invalid for its IPv4 datagram.")
        elif require(transport, header, layer) and header > 20:
            result["notes"].append("TCP options are present and uninterpreted.")
    return result


def analyze(file, size, packets, header_bytes, expected):
    if not 1 <= packets <= 512 or not 64 <= header_bytes <= 65536:
        raise AnalysisRefused("analysis options exceed bounded limits")
    container, identity = checked_identity(expected)

    class AnalyzingReader(container.Reader):
        def __init__(self):
            super().__init__(file, size)
            self.pending, self.rows = None, []

        def packet(self, entry, interface):
            if entry["number"] > packets:
                return
            count = min(entry["capturedLength"], header_bytes)
            self.pending = (entry, interface, count, bytearray())
            if count == 0:
                self.finish_packet()

        def finish_packet(self):
            entry, interface, _count, prefix = self.pending
            self.rows.append(decode(entry, interface, bytes(prefix)))
            self.pending = None

        def read(self, count):
            start = self.offset
            data = super().read(count)
            if self.pending is not None:
                entry, _interface, maximum, prefix = self.pending
                left = int(entry["dataOffset"])
                first, last = max(start, left), min(self.offset, left + maximum)
                if first < last:
                    prefix.extend(data[first - start:last - start])
                if self.offset >= left + maximum:
                    self.finish_packet()
            return data

    reader = AnalyzingReader()
    index = container.index_reader(reader)
    if reader.pending is not None or file.read(1):
        raise AnalysisRefused("input extent or packet observation is inconsistent")
    result = {"decoder": identity, "options": {"packetLimit": packets, "headerBytes": header_bytes},
              "index": index, "packets": reader.rows,
              "sampleComplete": int(index["packetCount"]) == len(reader.rows), "limitations": LIMITATIONS}
    encoded = json.dumps(result, separators=(",", ":")).encode()
    if len(encoded) > MAX_OUTPUT:
        raise AnalysisRefused("typed analysis output budget exceeded")
    return encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["decode"])
    parser.add_argument("--packets", type=int, required=True)
    parser.add_argument("--header-bytes", type=int, required=True)
    parser.add_argument("--decoder-sha256", required=True)
    parser.add_argument("--container-sha256", required=True)
    parser.add_argument("--schema-sha256", required=True)
    args = parser.parse_args()
    try:
        extent = os.environ.get("WES_BYTE_INPUT_BYTES", "")
        if not re.fullmatch(r"0|[1-9][0-9]{0,19}", extent):
            raise AnalysisRefused("native byte extent is absent")
        expected = {"codeSha256": args.decoder_sha256, "containerSha256": args.container_sha256,
                    "schemaSha256": args.schema_sha256}
        result = analyze(sys.stdin.buffer, int(extent), args.packets, args.header_bytes, expected)
        sys.stdout.buffer.write(result + b"\n")
        sys.stdout.buffer.flush()
    except (ValueError, OSError, KeyError, struct.error):
        # No path, payload or traceback on the provider's error channel.
        print("Offline analysis refused: input, options or decoder identity is invalid or unavailable.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
