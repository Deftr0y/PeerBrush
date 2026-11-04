"""Credential-free native image smoke; synthetic unsigned data, no app launch.

This establishes hdiutil/symlink/mode round trips, not publisher signing,
notarization, installation, quarantine handling or release readiness.
"""
import pathlib
import shutil
import sys
import tempfile
import unittest

from package_macos_dmg import APP, SUPPORT, create_image


@unittest.skipUnless(sys.platform == 'darwin', 'Requires actual macOS hdiutil')
class NativeImage(unittest.TestCase):
    def test_synthetic_unsigned_payload_mounts_read_only_with_exact_content(self):
        # Retain failed work if the OS cannot detach; do not recursively clean a
        # mounted filesystem from a test's TemporaryDirectory finalizer either.
        root = pathlib.Path(tempfile.mkdtemp(prefix='peerbrush-dmg-smoke-')).resolve()
        payload = root / 'payload'
        app = payload / APP / 'Contents/MacOS'
        app.mkdir(parents=True)
        executable = app / 'peerbrush'
        executable.write_bytes(b'SYNTHETIC_UNSIGNED_DATA_NEVER_EXECUTED\n')
        executable.chmod(0o755)
        support = payload / SUPPORT
        support.mkdir()
        (support / 'LICENSE').write_text('Synthetic GPL notice fixture\n')
        (payload / 'Applications').symlink_to('/Applications', target_is_directory=True)
        def check_read_only(mounted_app):
            with self.assertRaises(OSError):
                (mounted_app / 'Contents/MacOS/peerbrush').write_bytes(b'MUST_NOT_REPLACE')
        checked = create_image(payload, root / 'synthetic-unsigned.dmg', root / 'mounted',
                               'PeerBrush synthetic QA', verify_app=check_read_only)
        self.assertEqual(checked['Applications'], {'link': '/Applications'})
        self.assertEqual(checked[f'{APP}/Contents/MacOS/peerbrush']['mode'], 0o755)
        self.assertFalse((root / 'mounted').is_mount())
        shutil.rmtree(root)


if __name__ == '__main__':
    unittest.main()
