#!/usr/bin/env python3
"""Finite local owned-stdin export; exclusive publication, no overwrite or retry."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import stat
import sys

MAXIMUM = 536870912


def interrupted(_signum, _frame):
    raise InterruptedError("export cancelled")


def owned_entry(parent, name, identity):
    try:
        current = os.stat(name, dir_fd=parent, follow_symlinks=False)
    except FileNotFoundError:
        return False
    return stat.S_ISREG(current.st_mode) and (current.st_dev, current.st_ino) == (identity.st_dev, identity.st_ino)


def export(destination, source, extent):
    if not destination or len(destination.encode()) > 4096 or any(ord(c) < 32 or ord(c) == 127 for c in destination):
        raise ValueError("invalid destination")
    path = Path(destination)
    if ".." in path.parts or path.name in ("", ".", ".."):
        raise ValueError("invalid destination")
    absolute = Path(os.path.abspath(path))
    # Resolve directories by owned descriptors; no path component can redirect a write.
    parent = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    temporary = None
    identity = None
    published = False
    try:
        for component in absolute.parent.parts[1:]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
            os.close(parent)
            parent = child
        owner = os.fstat(parent)
        if owner.st_uid != os.getuid() or owner.st_mode & 0o022:
            raise ValueError("destination parent must be user-owned and not group/world writable")
        candidate = ".wes-export-" + secrets.token_hex(16)
        descriptor = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
        temporary = candidate
        identity = os.fstat(descriptor)
        digest = hashlib.sha256()
        with os.fdopen(descriptor, "wb") as output:
            remaining = extent
            while remaining:
                chunk = source.read(min(remaining, 65536))
                if not chunk:
                    raise ValueError("input ended before its acknowledged extent")
                output.write(chunk)
                digest.update(chunk)
                remaining -= len(chunk)
            if source.read(1):
                raise ValueError("input exceeds its acknowledged extent")
            output.flush()
            os.fsync(output.fileno())
            identity = os.fstat(output.fileno())
        # link is atomic and exclusive, including against symlinks and concurrent writers.
        if not owned_entry(parent, temporary, identity):
            raise ValueError("owned temporary identity changed before publication")
        os.link(temporary, absolute.name, src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
        published = True
        if not owned_entry(parent, absolute.name, identity):
            raise RuntimeError("published identity could not be acknowledged")
        if owned_entry(parent, temporary, identity):
            os.unlink(temporary, dir_fd=parent)
        temporary = None
        os.fsync(parent)
        return {"file": str(absolute), "bytes": str(extent), "sha256": "sha256:" + digest.hexdigest(),
                "device": str(identity.st_dev), "inode": str(identity.st_ino), "published": True}
    except BaseException as error:
        # A published target survives missing/failed acknowledgement; never erase or replay it.
        if published:
            raise RuntimeError("export may be published; inspect the explicit destination before any new attempt") from error
        raise
    finally:
        if temporary is not None and identity is not None:
            try:
                if owned_entry(parent, temporary, identity):
                    os.unlink(temporary, dir_fd=parent)
            except FileNotFoundError:
                pass
        os.close(parent)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file", required=True)
    parser.add_argument("--allow-confidential", action="store_true")
    args = parser.parse_args()
    extent = os.environ.get("WES_BYTE_INPUT_BYTES", "")
    if not extent.isascii() or not extent.isdecimal() or len(extent) > 12 or int(extent) > MAXIMUM:
        raise ValueError("missing or invalid acknowledged native byte extent")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    print(json.dumps(export(args.file, sys.stdin.buffer, int(extent)), separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
