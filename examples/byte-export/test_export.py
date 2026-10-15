"""Synthetic file export failures and exclusive publication; no live inputs."""
import hashlib
import io
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import export_bytes as writer


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.target = self.root / "result.bin"

    def write(self, payload=b"synthetic\x00\xff", extent=None):
        return writer.export(str(self.target), io.BytesIO(payload), len(payload) if extent is None else extent)

    def test_exact_bytes_empty_and_receipt(self):
        for payload in [b"", b"synthetic\x00\xff" * 65536]:
            receipt = self.write(payload)
            self.assertEqual(self.target.read_bytes(), payload)
            self.assertEqual(receipt["bytes"], str(len(payload)))
            self.assertEqual(receipt["sha256"], "sha256:" + hashlib.sha256(payload).hexdigest())
            self.assertEqual(int(receipt["inode"]), self.target.stat().st_ino)
            self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), 0o600)
            self.target.unlink()
            self.assertEqual(list(self.root.iterdir()), [])

    def test_existing_file_symlink_and_directory_never_overwritten(self):
        outside = self.root / "outside"
        outside.write_bytes(b"original")
        for kind in ["file", "symlink", "directory"]:
            if kind == "file": self.target.write_bytes(b"original")
            elif kind == "symlink": self.target.symlink_to(outside)
            else: self.target.mkdir()
            with self.assertRaises(FileExistsError): self.write()
            self.assertEqual(outside.read_bytes(), b"original")
            self.assertFalse(list(self.root.glob(".wes-export-*")))
            if kind == "directory": self.target.rmdir()
            else: self.target.unlink()

    def test_symlink_directory_components_and_traversal_refuse(self):
        directory = self.root / "actual"
        directory.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(directory, target_is_directory=True)
        for target in [alias / "file", alias / "nested" / "file", self.root / ".." / "file"]:
            with self.assertRaises((ValueError, OSError)):
                writer.export(str(target), io.BytesIO(b"x"), 1)
        self.assertEqual(list(directory.iterdir()), [])

    def test_untrusted_parent_mode_refuses(self):
        os.chmod(self.root, 0o777)
        self.addCleanup(os.chmod, self.root, 0o700)
        with self.assertRaises(ValueError): self.write()
        self.assertFalse(self.target.exists())

    def test_short_or_excess_input_cleans_unpublished_temporary(self):
        for size in [0, 2]:
            with self.assertRaises(ValueError): self.write(b"x", size)
            self.assertEqual(list(self.root.iterdir()), [])

    def test_input_failure_and_cancellation_clean_temporary(self):
        for error in [OSError("synthetic source failure"), InterruptedError("synthetic cancellation")]:
            source = unittest.mock.Mock()
            source.read.side_effect = error
            with self.assertRaises(type(error)):
                writer.export(str(self.target), source, 1)
            self.assertEqual(list(self.root.iterdir()), [])

    def test_failed_file_sync_does_not_publish(self):
        with patch.object(writer.os, "fsync", side_effect=OSError("synthetic sync failure")):
            with self.assertRaises(OSError): self.write()
        self.assertEqual(list(self.root.iterdir()), [])

    def test_failed_directory_sync_preserves_published_target(self):
        real_sync = os.fsync
        def sync(fd):
            if stat.S_ISDIR(os.fstat(fd).st_mode): raise OSError("synthetic directory failure")
            real_sync(fd)
        with patch.object(writer.os, "fsync", side_effect=sync):
            with self.assertRaisesRegex(RuntimeError, "may be published"): self.write()
        self.assertEqual(self.target.read_bytes(), b"synthetic\x00\xff")
        with self.assertRaises(FileExistsError): self.write()
        self.assertFalse(list(self.root.glob(".wes-export-*")))

    def test_concurrent_publication_never_replaces_winner(self):
        real_link = os.link
        def competing(*args, **kwargs):
            self.target.write_bytes(b"synthetic winner")
            return real_link(*args, **kwargs)
        with patch.object(writer.os, "link", side_effect=competing):
            with self.assertRaises(FileExistsError): self.write()
        self.assertEqual(self.target.read_bytes(), b"synthetic winner")
        self.assertFalse(list(self.root.glob(".wes-export-*")))

    def test_temporary_name_collision_never_removes_a_foreign_entry(self):
        foreign = self.root / ".wes-export-occupied"
        foreign.write_bytes(b"synthetic foreign entry")
        with patch.object(writer.secrets, "token_hex", return_value="occupied"):
            with self.assertRaises(FileExistsError): self.write()
        self.assertEqual(foreign.read_bytes(), b"synthetic foreign entry")
        self.assertFalse(self.target.exists())

    def test_swapped_temporary_is_neither_published_nor_removed(self):
        real_sync = os.fsync
        foreign = self.root / ".wes-export-owned"
        def swapped(fd):
            if not stat.S_ISDIR(os.fstat(fd).st_mode):
                foreign.rename(self.root / "original")
                foreign.write_bytes(b"synthetic replacement")
            real_sync(fd)
        with patch.object(writer.secrets, "token_hex", return_value="owned"), patch.object(writer.os, "fsync", side_effect=swapped):
            with self.assertRaisesRegex(ValueError, "identity changed"): self.write()
        self.assertFalse(self.target.exists())
        self.assertEqual(foreign.read_bytes(), b"synthetic replacement")

    def test_swap_during_link_cannot_acknowledge_a_foreign_file(self):
        real_link = os.link
        foreign = self.root / ".wes-export-owned"
        def swapped(*args, **kwargs):
            foreign.rename(self.root / "original")
            foreign.write_bytes(b"synthetic replacement")
            return real_link(*args, **kwargs)
        with patch.object(writer.secrets, "token_hex", return_value="owned"), patch.object(writer.os, "link", side_effect=swapped):
            with self.assertRaisesRegex(RuntimeError, "may be published"): self.write()
        self.assertEqual(self.target.read_bytes(), b"synthetic replacement")
        self.assertEqual(foreign.read_bytes(), b"synthetic replacement")

    def test_missing_or_invalid_native_extent_refuses(self):
        for extent in ["", "-1", "01x", "536870913", "1" * 1000, "١"]:
            result = subprocess.run([sys.executable, writer.__file__, "--file", str(self.target)], input=b"x", capture_output=True, env={**os.environ, "WES_BYTE_INPUT_BYTES": extent})
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.target.exists())

    def test_acknowledgement_failure_leaves_published_target(self):
        with patch.object(sys, "argv", [writer.__file__, "--file", str(self.target)]), patch.dict(os.environ, {"WES_BYTE_INPUT_BYTES": "1"}), patch.object(sys, "stdin") as source, patch("builtins.print", side_effect=BrokenPipeError):
            source.buffer = io.BytesIO(b"x")
            with self.assertRaises(BrokenPipeError): writer.main()
        self.assertEqual(self.target.read_bytes(), b"x")


if __name__ == "__main__":
    unittest.main()
