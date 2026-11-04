"""Public facts shared by the private test runner and its attesting DMG consumer."""
import re


CASES = ('no-update', 'network-error', 'cancel-check', 'cancel-download',
         'interrupted-before-quit', 'interrupted-install', 'success')


def validate(record, commit, version, archive_sha256, image_sha256, run=None):
    fields = {'schema', 'commit', 'version', 'starting_version', 'run_id', 'cases',
              'target_zip_sha256', 'target_dmg_sha256', 'starting_zip_sha256', 'starting_dmg_sha256'}
    if (not isinstance(record, dict) or set(record) != fields or type(record['schema']) is not int
            or record['schema'] != 1 or record['commit'] != commit or record['version'] != version
            or version != '0.30.0' or record['starting_version'] != '0.29.99'
            or record['cases'] != list(CASES) or type(record['run_id']) is not str
            or not re.fullmatch(r'[1-9][0-9]{0,19}', record['run_id']) or run is not None and record['run_id'] != run
            or record['target_zip_sha256'] != archive_sha256 or record['target_dmg_sha256'] != image_sha256
            or any(type(record[field]) is not str or not re.fullmatch('[0-9a-f]{64}', record[field])
                   for field in fields if field.endswith('sha256'))
            or len({record[field] for field in fields if field.endswith('sha256')}) != 4):
        raise ValueError('Signed two-version result does not match this source, both final packages and the complete exercise.')
    return record
