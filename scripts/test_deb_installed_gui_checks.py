"""Offline installed-GUI fixture contracts; never launch apps, displays or containers."""
import ast
import hashlib
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest import mock
import zlib

import test_deb_installed_gui as gui
import test_deb_version_upgrade as upgrade

ROOT = Path(__file__).resolve().parents[1]


def png(width, height, depth, pixels):
    def chunk(kind, value):
        return struct.pack('>I', len(value)) + kind + value + struct.pack('>I', zlib.crc32(kind + value))
    stride = width * 4 * (depth // 8)
    raw = b''.join(b'\0' + pixels[y * stride:(y + 1) * stride] for y in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, depth, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))


def screen(colors, *, thumbnails=False, only_thumbnails=False, swapped=False,
           size=(400., 300.), origin=(100., 100.)):
    """Pixel-center bilinear sampling of the actual fixed 32x24 quadrant texture."""
    width, height = gui.WINDOW_SIZE
    pixels = bytearray(bytes([41, 39, 43, 255]) * (width * height))
    def rectangle(left, top, right, bottom, color):
        for y in range(top, bottom):
            pixels[(y * width + left) * 4:(y * width + right) * 4] = bytes(color) * (right - left)
    if not only_thumbnails:
        left, top = origin
        right, bottom = left + size[0], top + size[1]
        start, end = math.ceil(left - 0.5), math.ceil(right - 0.5)
        sx, sy = size[0] / 32., size[1] / 24.
        weights = [max(0., min(1., (x + 0.5 - left) / sx - 15.5)) for x in range(start, end)]
        arranged = [colors[index ^ (1 if swapped else 0)] for index in range(4)]
        def row(vertical):
            return bytes(round((arranged[0][ch] * (1 - horizontal) + arranged[1][ch] * horizontal) * (1 - vertical)
                               + (arranged[2][ch] * (1 - horizontal) + arranged[3][ch] * horizontal) * vertical)
                         for horizontal in weights for ch in range(4))
        pure = [row(0.), row(1.)]
        for y in range(math.ceil(top - 0.5), math.ceil(bottom - 0.5)):
            vertical = max(0., min(1., (y + 0.5 - top) / sy - 11.5))
            data = pure[int(vertical)] if vertical in (0., 1.) else row(vertical)
            pixels[(y * width + start) * 4:(y * width + end) * 4] = data
    if thumbnails or only_thumbnails:
        for index, color in enumerate(colors):
            rectangle(1100, 100 + index * 25, 1120, 120 + index * 25, color)
    return width, height, 8, bytes(pixels)


class InstalledGuiContracts(unittest.TestCase):
    @unittest.skipUnless(os.name == 'posix', 'Container ownership/mode contract requires POSIX filesystems')
    def test_actual_owned_capture_persistence_overrides_restrictive_umask_inside_private_parent(self):
        tree = ast.parse(Path(gui.__file__).read_text(encoding='utf-8'))
        check = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'check')
        start = next(index for index, node in enumerate(check.body) if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id == 'captures' for target in node.targets))
        end = next(index for index, node in enumerate(check.body) if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id == 'receipt' for target in node.targets))
        statements = ast.fix_missing_locations(ast.Module(body=check.body[start:end], type_ignores=[]))
        with tempfile.TemporaryDirectory(prefix='peerbrush-owned-captures-') as temporary:
            previous = os.umask(0o077)
            try:
                parent = Path(temporary).resolve(strict=True) / 'private-evidence'
                parent.mkdir(mode=0o700)
                work = parent / 'owned-gui-work'
                work.mkdir(mode=0o700)
                output = gui.receipt_path(parent / 'installed-gui.json')
                names = [name for index in range(4) for name in [f'canvas-{index}.png', f'activation-{index}.png']] + \
                    ['settings-settled.png'] + [f'restart-canvas-{index}.png' for index in range(4)]
                images = {}
                captures = []
                for index, name in enumerate(names):
                    data = png(1, 1, 8, bytes([index, 37, 73, 255]))
                    (work / name).write_bytes(data)
                    images[name] = data
                    captures.append(dict(file=name, sha256=hashlib.sha256(data).hexdigest()))
                namespace = dict(output=output, work=work, result=dict(captures=captures), Path=Path,
                    stat=gui.stat, require=gui.require, bounded_file=gui.bounded_file)
                # Execute the actual persistence block, not the GUI/installer path.
                exec(compile(statements, '<actual-owned-capture-persistence>', 'exec'), namespace)
                retained = parent / 'installed-gui-captures'
                self.assertEqual(parent.stat().st_mode & 0o777, 0o700)
                self.assertEqual(retained.stat().st_mode & 0o777, 0o755)
                self.assertEqual({path.name for path in retained.iterdir()}, set(images))
                self.assertEqual(len(images), 13)
                for name, data in images.items():
                    self.assertEqual((retained / name).stat().st_mode & 0o777, 0o644)
                    self.assertEqual((retained / name).read_bytes(), data)
                    self.assertEqual((work / name).read_bytes(), data)
            finally:
                os.umask(previous)

    def test_actual_tiny_native_decoder_is_unchanged_by_explicit_fixed_capture_case(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gui-png-') as temporary:
            root = Path(temporary)
            for depth in [8, 16]:
                pixel = bytes([19, 37, 73, 255]) if depth == 8 else b'\x27\x11\x4e\x23\x75\x37\xff\xff'
                data = pixel * (32 * 24)
                path = root / ('native-' + str(depth) + '.png')
                path.write_bytes(png(32, 24, depth, data))
                self.assertEqual(upgrade.png_samples(path), (32, 24, depth, data))
            capture = root / 'window.png'
            expected = screen(gui.palette(0), thumbnails=True)
            capture.write_bytes(png(*expected))
            with self.assertRaises(ValueError):
                upgrade.png_samples(capture)
            self.assertEqual(upgrade.png_samples(capture, expected_size=gui.WINDOW_SIZE), expected)
            for invalid in [(32, 25), (1361, 900), (16000, 16000)]:
                with self.subTest(dimensions=invalid), self.assertRaises(ValueError):
                    upgrade.png_samples(capture, expected_size=invalid)
            damaged = bytearray(capture.read_bytes())
            damaged[-1] ^= 1
            capture.write_bytes(damaged)
            with self.assertRaises(ValueError):
                upgrade.png_samples(capture, expected_size=gui.WINDOW_SIZE)

    def test_canvas_oracle_requires_actual_large_arranged_canvas_and_ignores_thumbnails(self):
        for cell in range(4):
            colors = gui.palette(cell)
            self.assertEqual(gui.canvas_quadrants(screen(colors, thumbnails=True), colors),
                             [(100, 100, 293, 243), (306, 100, 499, 243),
                              (100, 256, 293, 399), (306, 256, 499, 399)])
            # Both integer and non-integer fit scales/origins create a real
            # magnified texel of mixed colors, much wider than four screen pixels.
            for size, origin in [((768., 576.), (100., 100.)),
                                 ((684., 513.), (100.25, 100.75)),
                                 ((721.5, 541.125), (100.125, 100.375))]:
                boxes = gui.canvas_quadrants(screen(colors, size=size, origin=origin), colors)
                self.assertGreater(boxes[1][0] - boxes[0][2], 4)
                self.assertGreater(boxes[2][1] - boxes[0][3], 4)
            for sample in [screen(colors, only_thumbnails=True), screen(colors, swapped=True),
                           screen(gui.palette((cell + 1) % 4)), screen(colors, size=(400., 240.))]:
                with self.subTest(cell=cell), self.assertRaises(ValueError):
                    gui.canvas_quadrants(sample, colors)
        # A hole in the blended seam must not pass merely because the allowed
        # geometric gap now follows the magnified source texel. Pure obstruction
        # must also fail the original region-density check.
        for left, top, right, bottom in [(299, 100, 302, 400), (150, 150, 220, 220)]:
            sample = screen(gui.palette(0))
            pixels = bytearray(sample[3])
            for y in range(top, bottom):
                pixels[(y * sample[0] + left) * 4:(y * sample[0] + right) * 4] = bytes([41, 39, 43, 255]) * (right - left)
            with self.subTest(obstruction=(left, top, right, bottom)), self.assertRaises(ValueError):
                gui.canvas_quadrants((*sample[:3], bytes(pixels)), gui.palette(0))
        with self.assertRaises(ValueError):
            gui.canvas_quadrants((1360, 900, 16, b''), gui.palette(0))

    def test_desktop_launch_preserves_unicode_arguments_and_does_not_wait_for_app_pipe_eof(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gui-launch-') as temporary:
            work = Path(temporary)
            desktop = work / 'installed.desktop'
            desktop.write_bytes(b'[Desktop Entry]\nExec=/usr/bin/peerbrush -- %F\n')
            window = gui.Window(dict(desktop=desktop.read_text()), work)
            window.pid = 2468
            path = work / 'Open With 作品 e\u0301 16.psb'
            with mock.patch.object(gui, 'DESKTOP', desktop), mock.patch.object(window, 'authenticate') as authenticate, \
                    mock.patch.object(gui.subprocess, 'run') as launch:
                window.launch([path])
            self.assertEqual(launch.call_args.args[0], ['gio', 'launch', str(desktop), str(path)])
            options = launch.call_args.kwargs
            self.assertIs(options['check'], True)
            self.assertIs(options['stdout'], options['stderr'])
            self.assertNotEqual(options['stdout'], subprocess.PIPE)
            self.assertTrue(options['stdout'].closed)
            self.assertNotIn('shell', options)
            authenticate.assert_called_once_with()

    def test_actual_process_consumer_rejects_changed_uid_media_route_or_parent(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gui-owner-') as temporary:
            work = Path(temporary)
            process = work / 'proc/2468'
            process.mkdir(parents=True)
            executable = process / 'exe'
            original = b'SYNTHETIC_NONEXECUTED_EXECUTABLE'
            executable.write_bytes(original)
            (process / 'status').write_text('Uid: 1000 1000 1000 1000\nPPid: 600\n')
            (process / 'cmdline').write_bytes(b'/usr/bin/peerbrush\0--\0Unicode.psb\0')
            (process / 'stat').write_text('2468 (PeerBrush) S ' + '0 ' * 18 + '12345')
            window = gui.Window(dict(expected=dict(executable_sha256=hashlib.sha256(original).hexdigest())), work)
            def selected(value):
                return work / 'proc' if value == '/proc' else executable if value == '/usr/bin/peerbrush' else Path(value)
            with mock.patch.object(gui, 'Path', side_effect=selected), \
                    mock.patch.object(gui.os, 'geteuid', return_value=1000, create=True), \
                    mock.patch.object(gui.os, 'getpid', return_value=600):
                self.assertEqual(window.process_identity(2468), '12345')
                for name, changed, restore in [
                    ('status', b'Uid: 9999 9999 9999 9999\nPPid: 600\n', (process / 'status').read_bytes()),
                    ('exe', b'CHANGED_BINARY', original),
                    ('cmdline', b'/usr/bin/peerbrush\0--headless\0', (process / 'cmdline').read_bytes()),
                ]:
                    (process / name).write_bytes(changed)
                    with self.subTest(boundary=name), self.assertRaises(ValueError):
                        window.process_identity(2468)
                    (process / name).write_bytes(restore)
                (process / 'status').write_text('Uid: 1000 1000 1000 1000\nPPid: 601\n')
                with self.assertRaises(ValueError):
                    window.process_identity(2468)

    def test_real_close_boundary_selects_only_owned_window_and_waits_exact_adopted_child(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gui-close-') as temporary:
            window = gui.Window({}, Path(temporary))
            window.pid, window.pidfd, window.identity = 2468, 77, '12345'
            rows = '0x123 0 9999 fixture Foreign\n0x456 0 2468 fixture PeerBrush\n'
            with mock.patch.object(window, 'process_identity', return_value='12345') as identity, \
                    mock.patch.object(gui.select, 'select', side_effect=[([], [], []), ([], [], []), ([77], [], [])]), \
                    mock.patch.object(gui, 'run', side_effect=[rows, '']) as command, \
                    mock.patch.object(gui.os, 'waitpid', return_value=(2468, 0), create=True) as wait, \
                    mock.patch.object(gui.os, 'close') as close:
                window.close()
            self.assertEqual(command.call_args_list, [mock.call(['wmctrl', '-lp']), mock.call(['wmctrl', '-ic', '0x456'])])
            self.assertEqual(identity.call_args_list, [mock.call(2468), mock.call(2468)])
            wait.assert_called_once_with(2468, 0)
            close.assert_called_once_with(77)
            self.assertIsNone(window.pid)

    def test_foreign_missing_ambiguous_or_timed_out_close_never_claims_lifetime_completion(self):
        for rows, timeout in [('0x123 0 9999 fixture Foreign\n', False),
                              ('0x123 0 2468 fixture One\n0x456 0 2468 fixture Two\n', False),
                              ('0x123 0 2468 fixture PeerBrush\n', True)]:
            with self.subTest(timeout=timeout), tempfile.TemporaryDirectory(prefix='peerbrush-gui-refuse-') as temporary:
                window = gui.Window({}, Path(temporary))
                window.pid, window.pidfd, window.identity = 2468, 77, '12345'
                with mock.patch.object(window, 'process_identity', return_value='12345'), \
                        mock.patch.object(gui.select, 'select', return_value=([], [], [])), \
                        mock.patch.object(gui, 'run', return_value=rows) as command, \
                        mock.patch.object(gui.os, 'waitpid', create=True) as wait, \
                        mock.patch.object(gui.os, 'close') as close, self.assertRaises(ValueError):
                    window.close()
                self.assertEqual(command.call_count, 2 if timeout else 1)
                wait.assert_not_called()
                close.assert_not_called()
                self.assertEqual((window.pid, window.pidfd), (2468, 77))

    def test_host_guard_precedes_package_profile_display_or_process_work(self):
        with mock.patch.object(gui, 'require_disposable', side_effect=ValueError('host blocked')), \
                mock.patch.object(gui, 'selected_package') as package, \
                mock.patch.object(gui, 'installed_identity') as installed, \
                mock.patch.object(gui, 'installed_runuser') as user, \
                mock.patch.object(gui.subprocess, 'run') as native, self.assertRaisesRegex(ValueError, 'host blocked'):
            gui.check(Path('/private/unused.deb'), {}, Path('/private/unused.json'))
        for boundary in [package, installed, user, native]:
            boundary.assert_not_called()

    def test_all_native_callers_run_gui_only_after_real_migration_without_altering_authority(self):
        for name in ['ci.yml', 'release.yml', 'linux-qualification.yml']:
            source = (ROOT / '.github/workflows' / name).read_text(encoding='utf-8')
            self.assertIn('python -B scripts/test_deb_installed_gui_checks.py' if name != 'linux-qualification.yml'
                          else 'python3 -B scripts/test_deb_installed_gui_checks.py', source)
            self.assertEqual(source.count('/checks/test_deb_installed_gui.py --package'), 1)
            self.assertLess(source.index('/checks/test_deb_version_upgrade.py --old-source-root'),
                            source.index('/checks/test_deb_installed_gui.py --package'))
            for tool in ['xvfb', 'xauth', 'dbus-x11', 'libglib2.0-bin', 'openbox', 'wmctrl', 'mesa-vulkan-drivers']:
                self.assertIn(tool, source)
        dormant = (ROOT / '.github/workflows/linux-qualification.yml').read_text(encoding='utf-8')
        self.assertEqual(dormant.count('if: ${{ false }}'), 2)
        inventory = json.loads((ROOT / 'scripts/package-files.json').read_bytes())['source']
        self.assertIn('scripts/test_deb_installed_gui.py', inventory)
        self.assertIn('scripts/test_deb_installed_gui_checks.py', inventory)


if __name__ == '__main__':
    unittest.main()
