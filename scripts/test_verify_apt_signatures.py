"""Native-result contract regressions; synthetic output is not OpenPGP evidence."""
import datetime
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

from prepare_site_release import sha
import verify_apt_signatures as signatures


PRIMARY = 'A' * 40
SIGNER = 'B' * 40
START = datetime.datetime(2026, 11, 7, 10, tzinfo=datetime.timezone.utc)
END = START + datetime.timedelta(days=7)
NOW = START + datetime.timedelta(days=1)
PUBLIC = b'-----BEGIN PGP PUBLIC KEY BLOCK-----\nSYNTHETIC_NOT_A_KEY\n-----END PGP PUBLIC KEY BLOCK-----\n'
KEYRING = b'SYNTHETIC_NOT_AN_EXPORTED_KEYRING'


def result(kind='01', **changes):
    fields = [SIGNER, '2026-11-07', str(int(START.timestamp())), '0', '4', '0', '22', '8', kind, PRIMARY]
    for key, value in changes.items():
        fields[int(key)] = value
    return ('[GNUPG:] GOODSIG ' + SIGNER[-16:] + ' Synthetic fixture\n[GNUPG:] VALIDSIG ' + ' '.join(fields) + '\n').encode()


def listing():
    created, expires = str(int(START.timestamp()) - 86400), str(int(END.timestamp()) + 86400)
    return (f'pub:-:4096:1:{PRIMARY[-16:]}:{created}:{expires}:::::cSC:\n'
            f'fpr:::::::::{PRIMARY}:\nuid:-::::::::Synthetic fixture:\n'
            f'sub:-:255:22:{SIGNER[-16:]}:{created}:{expires}:::::s:\n'
            f'fpr:::::::::{SIGNER}:\n').encode()


def inline(release):
    return (b'-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n' + release
            + b'-----BEGIN PGP SIGNATURE-----\n\nSYNTHETIC_NOT_A_SIGNATURE\n-----END PGP SIGNATURE-----\n')


def native(tool, arguments, work, label):
    if label in ('inspect-public', 'public-keys'):
        return listing()
    if label in ('import-public', 'import-keyring'):
        return b''
    if label == 'export-keyring':
        return KEYRING
    return result('01' if label == 'inrelease' else '00')


class SignatureContracts(unittest.TestCase):
    def test_exact_cleartext_and_one_native_signature_are_required(self):
        release = b'Origin: PeerBrush\nSHA256:\n hash 1 Packages\n'
        signatures.cleartext(inline(release), release)
        signatures.cleartext(inline(release).replace(b'\n', b'\r\n'), release)
        for data in [inline(release + b'changed\n'), b'prefix\n' + inline(release),
                     inline(release) + b'unsigned footer\n', inline(release) + b'-----END PGP SIGNATURE-----\n',
                     inline(release).replace(b'Hash: SHA256', b'Hash: SHA1')]:
            with self.subTest(data=data[:32]), self.assertRaises(ValueError):
                signatures.cleartext(data, release)
        self.assertEqual(signatures.status(result(), PRIMARY, START, END, NOW, '01'), SIGNER)
        for data in [result() + result(), result().replace(b'GOODSIG', b'UNEXPECTED'),
                     result() + b'[GNUPG:] REVKEYSIG rejected\n', b'']:
            with self.subTest(data=data[:32]), self.assertRaises(ValueError):
                signatures.status(data, PRIMARY, START, END, NOW, '01')

    def test_wrong_signer_weak_digest_class_and_signature_lifetime_fail(self):
        variants = [{'9': 'C' * 40}, {'7': '2'}, {'6': '17'}, {'8': '00'}, {'4': '6'},
                    {'2': str(int(START.timestamp()) - 1)}, {'2': str(int(NOW.timestamp()) + 1)},
                    {'3': str(int(END.timestamp()) - 1)}, {'3': '-1'}]
        for changes in variants:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                signatures.status(result(**changes), PRIMARY, START, END, NOW, '01')
        for bad in signatures.BAD_STATUS:
            with self.subTest(status=bad), self.assertRaises(ValueError):
                signatures.status(result() + ('[GNUPG:] ' + bad + ' rejected\n').encode(), PRIMARY, START, END, NOW, '01')

    def test_primary_and_actual_signing_key_must_be_usable_through_release_expiry(self):
        signatures.key_lifetimes(listing(), PRIMARY, [SIGNER], START, END, NOW)
        variants = [listing().replace(b'pub:-:', b'pub:r:'), listing().replace(b'sub:-:', b'sub:e:'),
            listing().replace(b':::::cSC:', b':::::cSCD:'), listing().replace(b':::::s:', b':::::e:'),
            listing().replace(b':4096:1:', b':1024:1:'), listing().replace(b':255:22:', b':128:22:'),
            listing().replace(str(int(END.timestamp()) + 86400).encode(), str(int(END.timestamp()) - 1).encode()),
            listing().replace(str(int(START.timestamp()) - 86400).encode(), str(int(START.timestamp()) + 1).encode()),
            listing().replace(SIGNER.encode(), ('C' * 40).encode())]
        for data in variants:
            with self.subTest(data=data[:32]), self.assertRaises(ValueError):
                signatures.key_lifetimes(data, PRIMARY, [SIGNER], START, END, NOW)
        with self.assertRaises(ValueError):
            signatures.key_lifetimes(listing(), PRIMARY, [SIGNER], START, END, END)

    def test_verifier_uses_only_selected_public_certificate_and_fixed_native_commands(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gpg-contract-') as temporary:
            root = pathlib.Path(temporary).resolve()
            tool = root / ('gpg.exe' if os.name == 'nt' else 'gpg')
            tool.write_bytes(b'SYNTHETIC_NOT_AN_EXECUTABLE')
            tool.chmod(0o755)
            work = root / 'work'
            work.mkdir(mode=0o700)
            release = b'Origin: PeerBrush\n'
            with mock.patch.object(signatures, 'run', side_effect=native) as run:
                record, keyring = signatures.verify(release, inline(release), b'fake detached', PUBLIC,
                    tool=tool, tool_sha256=sha(tool.read_bytes()), fingerprint=PRIMARY,
                    work=work, start=START, end=END, now=NOW)
            self.assertEqual(record['primary_fingerprint'], PRIMARY)
            self.assertEqual(record['signer_fingerprints'], [SIGNER])
            self.assertEqual(keyring, KEYRING)
            exported = work / signatures.KEYRING_NAME
            self.assertEqual(exported.read_bytes(), keyring)
            self.assertEqual([call.args[3] for call in run.call_args_list],
                             ['inspect-public', 'import-public', 'export-keyring', 'import-keyring',
                              'public-keys', 'inrelease', 'detached'])
            self.assertEqual(run.call_args_list[2].args[1][-4:],
                             ['--export-options', 'export-minimal', '--export', PRIMARY])
            self.assertEqual(run.call_args_list[3].args[1][-2:], ['--import', str(exported)])
            for call in run.call_args_list:
                _, arguments, home, _ = call.args
                self.assertEqual(arguments[0], '--no-options')
                for flag in ['--no-autostart', '--disable-dirmngr', '--no-auto-key-retrieve', '--no-auto-key-import']:
                    self.assertIn(flag, arguments)
                self.assertNotIn('--sign', arguments)
                self.assertEqual(arguments[2], str(home / 'keys'))
            self.assertTrue(all(call.args[2] == work / 'client' for call in run.call_args_list[3:]))

    def test_native_runner_hides_raw_failures_and_kills_timed_out_child(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gpg-runner-') as temporary:
            work = pathlib.Path(temporary).resolve()
            private = 'SYNTHETIC_PRIVATE_CANARY'
            with mock.patch.object(signatures.subprocess, 'Popen', side_effect=OSError(private)):
                with self.assertRaises(ValueError) as failure:
                    signatures.run(work / 'gpg', ['fixture'], work, 'failed')
            self.assertNotIn(private, str(failure.exception))
            child = mock.Mock()
            child.poll.return_value = None
            with mock.patch.object(signatures.subprocess, 'Popen', return_value=child) as process, \
                    mock.patch.object(signatures.time, 'monotonic', side_effect=[0, 31]):
                with self.assertRaises(ValueError):
                    signatures.run(work / 'gpg', ['fixture'], work, 'timeout')
            child.kill.assert_called_once()
            child.wait.assert_called_once_with(timeout=10)
            self.assertEqual(process.call_args.kwargs['stdin'], subprocess.DEVNULL)
            self.assertEqual(process.call_args.kwargs['stderr'], subprocess.DEVNULL)
            self.assertEqual(process.call_args.kwargs['env']['GNUPGHOME'], str(work / 'keys'))

    def test_private_material_and_changed_tool_fail_before_public_import(self):
        with tempfile.TemporaryDirectory(prefix='peerbrush-gpg-preflight-') as temporary:
            root = pathlib.Path(temporary).resolve()
            tool = root / ('gpg.exe' if os.name == 'nt' else 'gpg')
            tool.write_bytes(b'SYNTHETIC_NOT_AN_EXECUTABLE')
            tool.chmod(0o755)
            release = b'Origin: PeerBrush\n'
            for index, (key, tool_hash, preview) in enumerate([
                    (PUBLIC.replace(b'PUBLIC KEY', b'PRIVATE KEY'), sha(tool.read_bytes()), listing()),
                    (PUBLIC, '0' * 64, listing()),
                    (PUBLIC, sha(tool.read_bytes()), listing().replace(b'pub:', b'sec:'))]):
                work = root / str(index)
                work.mkdir(mode=0o700)
                with mock.patch.object(signatures, 'run', return_value=preview) as run, self.assertRaises(ValueError):
                    signatures.verify(release, inline(release), b'fake detached', key,
                        tool=tool, tool_sha256=tool_hash, fingerprint=PRIMARY,
                        work=work, start=START, end=END, now=NOW)
                self.assertNotIn('import-public', [call.args[3] for call in run.call_args_list])


if __name__ == '__main__':
    unittest.main()
