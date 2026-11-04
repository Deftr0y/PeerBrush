"""File identity regressions; Windows ctime sources may differ, file IDs may not."""
import hashlib
import os
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import package_macos_dmg as dmg


FIELDS = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns", "st_mode", "st_nlink")


def metadata(info, **changes):
    values = {field: getattr(info, field) for field in FIELDS}
    values["st_birthtime_ns"] = getattr(info, "st_birthtime_ns", 101)
    return SimpleNamespace(**dict(values, **changes))


class FileIdentity(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="peerbrush-file-id-")
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name).resolve() / "synthetic"
        self.data = b"bounded synthetic input"
        self.path.write_bytes(self.data)
        # Explicit nonzero identities make these tests deterministic even on a
        # platform whose temporary filesystem does not supply a real inode.
        self.before = metadata(self.path.lstat(), st_dev=17, st_ino=23, st_ctime_ns=101)
        self.opened = metadata(self.before, st_ctime_ns=202)

    def read(self, before=None, opened=None, after_path=None, after_handle=None, platform="nt"):
        before = self.before if before is None else before
        opened = self.opened if opened is None else opened
        after_path = before if after_path is None else after_path
        after_handle = opened if after_handle is None else after_handle
        # No registration, native API override, symlink or host file is needed.
        # The actual file read remains bounded; only metadata is synthetic.
        with mock.patch.object(dmg, "inspection_path", return_value=self.path), \
                mock.patch.object(Path, "lstat", side_effect=[before, after_path]), \
                mock.patch.object(dmg.os, "fstat", side_effect=[opened, after_handle]), \
                mock.patch.object(dmg.os, "name", platform):
            return dmg.bounded_file(self.path, 1024)

    def test_windows_ctime_representation_may_differ_when_shared_identity_matches(self):
        self.assertEqual(self.read(), (hashlib.sha256(self.data).hexdigest(), len(self.data)))

    def test_each_shared_path_and_handle_identity_field_must_match(self):
        for field, value in [("st_dev", 18), ("st_ino", 24), ("st_size", len(self.data) + 1),
                             ("st_mtime_ns", self.before.st_mtime_ns + 1),
                             ("st_mode", stat.S_IFREG | 0o444), ("st_nlink", 2),
                             ("st_birthtime_ns", 303)]:
            if value == getattr(self.before, field):
                value += 1
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "unchanged bounded regular"):
                self.read(opened=metadata(self.opened, **{field: value}))

    def test_windows_requires_nonzero_shared_file_id(self):
        with self.assertRaisesRegex(ValueError, "unchanged bounded regular"):
            self.read(before=metadata(self.before, st_ino=0), opened=metadata(self.opened, st_ino=0))

    def test_posix_still_requires_matching_cross_source_ctime(self):
        with self.assertRaisesRegex(ValueError, "unchanged bounded regular"):
            self.read(platform="posix")
        self.assertEqual(self.read(opened=self.before, platform="posix")[1], len(self.data))

    def test_each_ctime_source_must_remain_stable_through_the_read(self):
        for changes in [dict(after_path=metadata(self.before, st_ctime_ns=303)),
                        dict(after_handle=metadata(self.opened, st_ctime_ns=303))]:
            with self.subTest(side=next(iter(changes))), self.assertRaisesRegex(ValueError, "changed during its bounded read"):
                self.read(**changes)

    def test_path_replacement_or_open_handle_mutation_cannot_pass(self):
        for field in ["st_dev", "st_ino", "st_size", "st_mtime_ns", "st_nlink", "st_birthtime_ns"]:
            for side, initial in [("after_path", self.before), ("after_handle", self.opened)]:
                changed = metadata(initial, **{field: getattr(initial, field) + 1})
                with self.subTest(field=field, side=side), self.assertRaisesRegex(ValueError, "changed during its bounded read"):
                    self.read(**{side: changed})

    def test_real_regular_input_is_read_without_stat_normalization_fixtures(self):
        self.assertEqual(dmg.bounded_file(self.path, 1024), (hashlib.sha256(self.data).hexdigest(), len(self.data)))


if __name__ == "__main__":
    unittest.main()
