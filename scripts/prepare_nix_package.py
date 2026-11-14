"""Prepare an unpublished Nix source package from exact public release bytes.

No Nix evaluation/build, toolchain qualification, install, catalog submission,
update, rollback or publication. Generated definitions require native acceptance.
"""
import argparse
import base64
import json
import pathlib
import re
import tomllib

from final_release import read
from package import portable_files, source_files
import prepare_site_release as release
from production_readiness import unique_fields


TEMPLATE = 'packaging/nix/package.nix.in'
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
TOOL_ATTRIBUTES = {'clang': 'llvmPackages_18', 'cmake': 'cmake', 'ninja': 'ninja'}


def validate_lock(data):
    if not 0 < len(data) <= 4 * 1024 * 1024:
        raise ValueError('Select a bounded Cargo lockfile.')
    value = tomllib.loads(data.decode('utf-8'))
    packages = value.get('package')
    if not isinstance(packages, list) or not 1 <= len(packages) <= 4096:
        raise ValueError('Nix packaging requires a bounded complete Cargo lockfile.')
    found = set()
    crates = {}
    for package in packages:
        if (not isinstance(package, dict) or not isinstance(package.get('name'), str) or not package['name']
                or not isinstance(package.get('version'), str) or not package['version']):
            raise ValueError('Cargo lockfile package identity is incomplete.')
        source = package.get('source')
        key = package['name'], package['version'], source
        if source is not None:
            checksum = package.get('checksum')
            if source != REGISTRY or not isinstance(checksum, str) or not re.fullmatch('[0-9a-f]{64}', checksum):
                raise ValueError('Nix packaging requires locked crates.io hashes; Git/alternate registries need a reviewed fetch adapter.')
            if (not re.fullmatch(r'[A-Za-z0-9_-]+', package['name'])
                    or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.+-]+)?', package['version'])):
                raise ValueError('Cargo lockfile contains an invalid registry crate name or version.')
            crates[f'{package["name"]}-{package["version"]}'] = checksum
        if key in found:
            raise ValueError('Cargo lockfile has duplicate package records.')
        found.add(key)
    return crates


def prepare(root, inventory, tag, commit, output, *, nixpkgs_commit, tool_nixpkgs_commits=None,
            get=release.public_get):
    if not isinstance(nixpkgs_commit, str) or not re.fullmatch('[0-9a-f]{40}', nixpkgs_commit):
        raise ValueError('Select one full reviewed Nixpkgs commit; mutable refs are not accepted.')
    pins = {} if tool_nixpkgs_commits is None else tool_nixpkgs_commits
    if (not isinstance(pins, dict) or not pins.keys() <= TOOL_ATTRIBUTES.keys()
            or any(not isinstance(pin, str) or not re.fullmatch('[0-9a-f]{40}', pin) for pin in pins.values())):
        raise ValueError('Native Nix tools require full reviewed Nixpkgs commits for Clang, CMake or Ninja.')
    tool_pins = {tool: pins.get(tool, nixpkgs_commit) for tool in TOOL_ATTRIBUTES}
    output = release.output_path(root, pathlib.Path(output))
    entry, _ = release.published(root, inventory, tag, commit,
        dict(notes=['Nix source package preparation.'], caution='Unpublished definition; native Nix acceptance pending.'), get)
    sources = source_files(root, commit)
    inputs = {name: data for name, data, _ in sources}
    required = {TEMPLATE, 'Cargo.lock', 'rust-toolchain.toml', 'scripts/jxl_build.py', 'scripts/package.py',
                'packaging/linux/com.peerbrush.PeerBrush.desktop', 'packaging/linux/com.peerbrush.PeerBrush.xml'}
    if not required <= set(inputs):
        raise ValueError('Selected source does not contain the complete Nix/native build contract.')
    validate_lock(inputs['Cargo.lock'])
    toolchain = tomllib.loads(inputs['rust-toolchain.toml'].decode('utf-8')).get('toolchain')
    rust = toolchain.get('channel') if isinstance(toolchain, dict) else None
    if not isinstance(rust, str) or not re.fullmatch(release.STABLE, rust):
        raise ValueError('Nix packaging requires an exact stable Rust toolchain pin.')
    version = tag.removeprefix('v')
    base = inventory.get('base_inventory', inventory)
    records = release.validate_inventory(base, commit, version)
    source = records[f'PeerBrush-{version}-source.zip']
    source_hash = 'sha256-' + base64.b64encode(bytes.fromhex(source['sha256'])).decode('ascii')
    template = inputs[TEMPLATE].decode('utf-8')
    values = {'VERSION': version, 'COMMIT': commit, 'SOURCE_URL': entry['sourceUrl'],
              'SOURCE_HASH': source_hash, 'RUST_VERSION': rust}
    for name, value in values.items():
        marker = '@' + name + '@'
        if marker not in template:
            raise ValueError('Nix package template is missing a required input binding.')
        template = template.replace(marker, value)
    if re.search('@[A-Z_]+@', template):
        raise ValueError('Nix package template contains an unresolved binding.')
    # Tool snapshots can differ from the Rust/runtime package set. All still
    # pass the exact native version probe when the derivation builds.
    overrides = ''.join(f'  {attr} = (importPinned "{tool_pins[tool]}").{attr};\n'
                        for tool, attr in TOOL_ATTRIBUTES.items() if tool_pins[tool] != nixpkgs_commit)
    default = ('let\n  importPinned = rev: import (builtins.fetchGit {\n'
        '    url = "https://github.com/NixOS/nixpkgs.git";\n'
        '    inherit rev;\n    shallow = true;\n    exportIgnore = false;\n'
        '  }) { system = "x86_64-linux"; config = {}; overlays = []; };\n'
        f'  pkgs = importPinned "{nixpkgs_commit}";\n'
        'in pkgs.callPackage ./package.nix {\n' + overrides + '}\n')
    flake = b'''{
  description = "PeerBrush, a native collaborative image editor";
  outputs = { self }: {
    packages.x86_64-linux = rec {
      peerbrush = import ./default.nix;
      default = peerbrush;
    };
  };
}
'''
    metadata = dict(schema=1, format='PeerBrushNixSourceInputsV1', version=version, source_commit=commit,
        source_url=entry['sourceUrl'], source_sha256=source['sha256'], nixpkgs_commit=nixpkgs_commit,
        native_tool_nixpkgs_commits=tool_pins,
        cargo_lock_sha256=release.sha(inputs['Cargo.lock']), rust_toolchain=rust,
        embedded_git_provenance='unknown-for-source-archive-build')
    files = {'flake.nix': flake, 'default.nix': default.encode('utf-8'), 'package.nix': template.encode('utf-8'),
             'Cargo.lock': inputs['Cargo.lock'], 'source-inputs.json': release.encode(metadata)}
    modes = {}
    for name, data, mode in portable_files(sources):
        files['support/' + name] = data
        modes['support/' + name] = mode
    report = dict(schema=1, format='PeerBrushNixPackagePreparationV1', version=version, commit=commit,
        release_url=entry['releaseUrl'], release_inventory_sha256=entry['releaseInventorySha256'],
        inputs=metadata, definition_prepared=True, platforms=['x86_64-linux'],
        nixpkgs_fetch_verification='not-performed', evaluation='not-performed', build='not-performed',
        native_toolchain_qualification='not-performed', reproducibility='not-established',
        installed_acceptance='not-performed', upgrade_rollback_acceptance='not-performed',
        catalog_publication='not-performed', release_ready=False,
        files={name: release.sha(data) for name, data in files.items()},
        executable_files=sorted(name for name, mode in modes.items() if mode & 0o111))
    output = release.output_path(root, output)
    output.mkdir(mode=0o700)
    for name, data in {**files, 'nix-package.json': release.encode(report)}.items():
        path = output / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open('xb') as stream:
            stream.write(data)
        path.chmod(modes.get(name, 0o644))
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['inventory', 'output']:
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    for name in ['inventory-sha256', 'tag', 'commit', 'nixpkgs-commit']:
        parser.add_argument('--' + name, required=True)
    for tool in TOOL_ATTRIBUTES:
        parser.add_argument(f'--{tool}-nixpkgs-commit', help=f'Optional full Nixpkgs commit for {tool}.')
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{64}', args.inventory_sha256):
        parser.error('Select the public release inventory with an independent SHA-256.')
    inventory = json.loads(read(args.inventory, 128 * 1024, args.inventory_sha256), object_pairs_hook=unique_fields)
    prepare(pathlib.Path(__file__).resolve().parents[1], inventory, args.tag, args.commit,
            args.output, nixpkgs_commit=args.nixpkgs_commit,
            tool_nixpkgs_commits={tool: pin for tool in TOOL_ATTRIBUTES
                                 if (pin := getattr(args, f'{tool}_nixpkgs_commit')) is not None})
    print('Unpublished Nix source package definition prepared; evaluation, qualified native build and installed acceptance remain pending.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
