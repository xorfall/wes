#!/usr/bin/env python3
"""Offline container index and original bytes; no capture backend or protocol dissector."""
import argparse
import hashlib
import json
import os
import stat
import struct
import sys

CHUNK = 65536
MAX_FILE = 512 * 1024 * 1024
MAX_RECORDS = 2_000_000
MAX_INTERFACES = 256
MAX_SECTIONS = 64
INDEX_PACKETS = 512
HEADER_LIMIT = 256 * 1024
MAGIC_NG = b"\x0a\x0d\x0d\x0a"
MAGIC_PCAP = {
    b"\xd4\xc3\xb2\xa1": ("<", 6), b"\xa1\xb2\xc3\xd4": (">", 6),
    b"\x4d\x3c\xb2\xa1": ("<", 9), b"\xa1\xb2\x3c\x4d": (">", 9),
}


class InvalidCapture(ValueError):
    pass


class Reader:
    def __init__(self, file, size):
        self.file, self.size, self.offset = file, size, 0
        self.digest = hashlib.sha256()

    def packet(self, entry, interface):
        """Optional sequential observer before packet data; the container owns all reads."""
        pass

    def read(self, size):
        if size < 0 or size > CHUNK or self.offset + size > self.size:
            raise InvalidCapture("truncated or invalid container range")
        data = self.file.read(size)
        if len(data) != size:
            raise InvalidCapture("input changed or ended during validation")
        self.offset += size
        self.digest.update(data)
        return data

    def skip(self, size):
        if size < 0 or self.offset + size > self.size:
            raise InvalidCapture("truncated container data")
        while size:
            n = min(size, CHUNK)
            self.read(n)
            size -= n

    def options(self, size, endian, timestamps=False):
        end, found, count = self.offset + size, {}, 0
        if size < 0 or size % 4:
            raise InvalidCapture("invalid option range")
        while self.offset < end:
            if end - self.offset < 4:
                raise InvalidCapture("truncated option header")
            code, length = struct.unpack(endian + "HH", self.read(4))
            count += 1
            if count > 4096 or self.offset + ((length + 3) & ~3) > end:
                raise InvalidCapture("option exceeds its bounded block")
            if code == 0:
                if length != 0 or self.offset != end:
                    raise InvalidCapture("invalid option terminator")
                break
            value = self.read(length)
            self.skip((-length) % 4)
            if timestamps and code in (9, 14):
                if code in found:
                    raise InvalidCapture("duplicate timestamp option")
                found[code] = value
        return found


def packet(index, interface, record, data, captured, original, ticks, reader=None):
    index["packetCount"] = str(int(index["packetCount"]) + 1)
    entry = {
        "number": int(index["packetCount"]), "interface": interface["id"],
        "recordOffset": str(record), "dataOffset": str(data),
        "capturedLength": captured, "originalLength": original,
        "timestampTicks": None if ticks is None else str(ticks),
    }
    if entry["number"] <= INDEX_PACKETS:
        index["packets"].append(entry)
    if reader is not None:
        reader.packet(entry, interface)


def classic_header(data):
    """Validate one original PCAP header; also used by the bounded live prefix reader."""
    if len(data) != 24 or data[:4] not in MAGIC_PCAP:
        raise InvalidCapture("unrecognized or truncated PCAP header")
    endian, power = MAGIC_PCAP[data[:4]]
    major, minor, _zone, _precision, snap, network = struct.unpack(endian + "HHiIII", data[4:])
    if (major, minor) != (2, 4) or snap == 0:
        raise InvalidCapture("only PCAP v2.4 with a nonzero snapshot length is supported")
    interface = {"id": 0, "section": 0, "linkType": network & 0xffff, "snapLength": snap,
                 "timestampScale": "10^" + str(power), "timestampOffset": "0"}
    return endian, power, interface


def classic_packet_header(data, endian, power, snap):
    if len(data) != 16:
        raise InvalidCapture("truncated PCAP packet header")
    sec, fraction, captured, original = struct.unpack(endian + "IIII", data)
    if fraction >= 10 ** power or captured > original or captured > snap:
        raise InvalidCapture("invalid PCAP timestamp or packet length")
    return captured, original, sec * 10 ** power + fraction


def classic(reader, magic, index):
    endian, power, interface = classic_header(magic + reader.read(20))
    index["interfaces"].append(interface)
    while reader.offset < reader.size:
        if int(index["packetCount"]) >= MAX_RECORDS:
            raise InvalidCapture("packet count limit exceeded")
        start = reader.offset
        captured, original, ticks = classic_packet_header(reader.read(16), endian, power, interface["snapLength"])
        packet(index, interface, start, reader.offset, captured, original, ticks, reader)
        reader.skip(captured)


def next_generation(reader, index):
    # First magic was already read. Every block, including unknown bodies, contributes to the hash.
    block_magic, endian, local, section, section_end, blocks = MAGIC_NG, None, [], -1, None, 0
    while True:
        start = reader.offset - 4
        blocks += 1
        if blocks > MAX_RECORDS:
            raise InvalidCapture("block count limit exceeded")
        length_bytes = reader.read(4)
        if block_magic == MAGIC_NG:
            if section_end is not None and start != section_end:
                raise InvalidCapture("declared section length does not match its boundary")
            bom = reader.read(4)
            endian = {b"\x4d\x3c\x2b\x1a": "<", b"\x1a\x2b\x3c\x4d": ">"}.get(bom)
            if endian is None:
                raise InvalidCapture("invalid PCAPNG byte order")
            length = struct.unpack(endian + "I", length_bytes)[0]
            kind = 0x0a0d0d0a
        else:
            if endian is None:
                raise InvalidCapture("missing section header")
            length = struct.unpack(endian + "I", length_bytes)[0]
            kind = struct.unpack(endian + "I", block_magic)[0]
        if length < 12 or length % 4 or start + length > reader.size:
            raise InvalidCapture("invalid or truncated block length")
        body_end = start + length - 4
        if kind == 0x0a0d0d0a:
            if length < 28:
                raise InvalidCapture("short section header")
            major, minor, declared = struct.unpack(endian + "HHq", reader.read(12))
            section += 1
            if (major, minor) != (1, 0) or section >= MAX_SECTIONS or declared < -1 or (declared >= 0 and declared % 4):
                raise InvalidCapture("unsupported section version, length or section count")
            section_end = None if declared == -1 else start + length + declared
            if section_end is not None and section_end > reader.size:
                raise InvalidCapture("declared section exceeds the file")
            local = []
            reader.options(body_end - reader.offset, endian)
        else:
            if section_end is not None and start + length > section_end:
                raise InvalidCapture("block exceeds its declared section")
            if kind == 1:
                if length < 20 or len(index["interfaces"]) >= MAX_INTERFACES:
                    raise InvalidCapture("short interface or interface count limit exceeded")
                link, _reserved, snap = struct.unpack(endian + "HHI", reader.read(8))
                options = reader.options(body_end - reader.offset, endian, timestamps=True)
                resolution, offset = options.get(9, b"\x06"), options.get(14, b"\0" * 8)
                if len(resolution) != 1 or len(offset) != 8:
                    raise InvalidCapture("invalid timestamp resolution or offset")
                r = resolution[0]
                interface = {"id": len(index["interfaces"]), "section": section, "linkType": link,
                             "snapLength": snap, "timestampScale": ("2^" if r & 128 else "10^") + str(r & 127),
                             "timestampOffset": str(struct.unpack(endian + "q", offset)[0])}
                local.append(interface)
                index["interfaces"].append(interface)
            elif kind == 6:
                if length < 32:
                    raise InvalidCapture("short enhanced packet")
                interface_id, high, low, captured, original = struct.unpack(endian + "IIIII", reader.read(20))
                if interface_id >= len(local):
                    raise InvalidCapture("packet references a missing interface")
                interface = local[interface_id]
                if captured > original or (interface["snapLength"] and captured > interface["snapLength"]):
                    raise InvalidCapture("invalid enhanced packet length")
                if reader.offset + ((captured + 3) & ~3) > body_end:
                    raise InvalidCapture("packet data exceeds its block")
                packet(index, interface, start, reader.offset, captured, original, (high << 32) | low, reader)
                reader.skip((captured + 3) & ~3)
                reader.options(body_end - reader.offset, endian)
            elif kind == 3:
                if length < 16 or not local:
                    raise InvalidCapture("simple packet requires the first interface")
                original = struct.unpack(endian + "I", reader.read(4))[0]
                interface = local[0]
                captured = min(original, interface["snapLength"]) if interface["snapLength"] else original
                if ((captured + 3) & ~3) != body_end - reader.offset:
                    raise InvalidCapture("simple packet data does not match its implied captured length")
                packet(index, interface, start, reader.offset, captured, original, None, reader)
                reader.skip((captured + 3) & ~3)
            elif kind == 2:
                raise InvalidCapture("obsolete PCAPNG Packet Blocks are unsupported")
            else:
                index["skippedBlocks"] += 1
                reader.skip(body_end - reader.offset)
        if reader.offset != body_end or struct.unpack(endian + "I", reader.read(4))[0] != length:
            raise InvalidCapture("block trailer length does not match its header")
        if reader.offset == reader.size:
            if section_end is not None and section_end != reader.offset:
                raise InvalidCapture("declared section is incomplete")
            return
        block_magic = reader.read(4)


def index_file(file, size):
    return index_reader(Reader(file, size))


def index_reader(reader):
    size = reader.size
    if not 4 <= size <= MAX_FILE or reader.offset != 0:
        raise InvalidCapture("file is empty, truncated or exceeds the 512 MiB import limit")
    magic = reader.read(4)
    index = {"format": "pcapng" if magic == MAGIC_NG else "pcap", "rawBytes": str(size), "sha256": "",
             "packetCount": "0", "indexComplete": True, "skippedBlocks": 0, "interfaces": [], "packets": []}
    if magic == MAGIC_NG:
        next_generation(reader, index)
    elif magic in MAGIC_PCAP:
        classic(reader, magic, index)
    else:
        raise InvalidCapture("unrecognized capture container")
    index["sha256"] = "sha256:" + reader.digest.hexdigest()
    index["indexComplete"] = int(index["packetCount"]) == len(index["packets"])
    return index


def signature(meta):
    return (meta.st_dev, meta.st_ino, meta.st_size, meta.st_mtime_ns, meta.st_ctime_ns)


def import_file(path, output):
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    with os.fdopen(os.open(path, flags), "rb") as file:
        before = os.fstat(file.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise InvalidCapture("import requires a regular file")
        index = index_file(file, before.st_size)
        if signature(before) != signature(os.fstat(file.fileno())):
            raise InvalidCapture("input changed during validation")
        header = json.dumps({"index": index, "raw": ""}, separators=(",", ":")).encode("utf-8")
        if len(header) > HEADER_LIMIT:
            raise InvalidCapture("capture index exceeds its header limit")
        file.seek(0)
        output.write(struct.pack(">I", len(header)))
        output.write(header)
        copied, digest = 0, hashlib.sha256()
        while copied < before.st_size:
            chunk = file.read(min(CHUNK, before.st_size - copied))
            if not chunk:
                raise InvalidCapture("input changed during import")
            digest.update(chunk)
            copied += len(chunk)
            output.write(chunk)
        if file.read(1) or signature(before) != signature(os.fstat(file.fileno())) or "sha256:" + digest.hexdigest() != index["sha256"]:
            raise InvalidCapture("input changed between validation and import; discard output")
        output.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["import"])
    parser.add_argument("--file", required=True)
    args = parser.parse_args()
    try:
        import_file(args.file, sys.stdout.buffer)
    except (InvalidCapture, OSError, struct.error):
        # Do not print paths, payloads or traceback. A failed process never publishes the raw draft.
        print("Capture import refused: malformed, unsupported, changed or unavailable regular file.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
