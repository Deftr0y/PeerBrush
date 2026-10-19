"""Production gates inspect real synthetic archives, without signing or publishing."""
import copy
import contextlib
import io
import json
import unittest

import test_package
from package import git
import production_readiness as readiness
from verify_release import verify


class Tests(unittest.TestCase):
    def setUp(self):
        self.fixture = test_package.Packages(methodName='runTest')
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.commit = git(self.fixture.root, 'rev-parse', 'HEAD').decode().strip()
        for platform in ['Windows', 'macOS', 'Linux']:
            self.fixture.build(platform)
        self.directory = self.fixture.root / 'verified'
        with contextlib.redirect_stdout(io.StringIO()):
            verify(self.fixture.output, self.commit, '0.2.0', self.directory, self.fixture.root)

    def assess(self, previous=None):
        return readiness.assess(self.directory, self.commit, '0.2.0', previous, self.fixture.root)

    def test_exact_source_candidates_never_claim_installed_or_signed_readiness(self):
        report = self.assess()
        self.assertTrue(report['candidate_verified'])
        self.assertFalse(report['production_ready'])
        self.assertEqual(set(report['platforms']), {'Windows', 'macOS', 'Linux'})
        for platform in report['platforms'].values():
            self.assertTrue(platform['candidate_verified'])
            self.assertFalse(platform['ready'])
            self.assertEqual(platform['public_availability'], 'unverified')

    def test_changed_missing_or_relabelled_bytes_fail(self):
        inventory_path = self.directory / 'release-inventory.json'
        original = inventory_path.read_bytes()
        inventory = json.loads(original)
        for field, value in [('commit', 'b' * 40), ('binary_source_commit', 'b' * 40), ('packaging_correction', '-repacked-20261019')]:
            inventory_path.write_text(json.dumps(dict(inventory, **{field: value})))
            with self.assertRaises(ValueError):
                self.assess()
        inventory_path.write_bytes(original)
        asset = self.directory / inventory['assets'][0]['file']
        asset.write_bytes(asset.read_bytes() + b'changed')
        with self.assertRaises(ValueError):
            self.assess()

    def test_checksums_and_immutable_release_version_are_enforced(self):
        inventory = json.loads((self.directory / 'release-inventory.json').read_text())
        self.assertEqual(self.assess(inventory)['immutable_version'], 'checked-against-supplied-inventory')
        for change in ['commit', 'hash', 'missing']:
            previous = copy.deepcopy(inventory)
            if change == 'commit':
                previous['commit'] = 'b' * 40
            elif change == 'hash':
                previous['assets'][0]['sha256'] = 'b' * 64
            else:
                previous['assets'].append(dict(file='old-release-file.zip', sha256='b' * 64))
            with self.assertRaises(ValueError):
                self.assess(previous)
        (self.directory / 'SHA256SUMS').write_text('changed')
        with self.assertRaises(ValueError):
            self.assess()


if __name__ == '__main__':
    unittest.main()
