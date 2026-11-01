"""Exact independent registry-versus-row primitive oracle. No application code.

Use an isolated CI executor for runtime qualification. This test never raises
document limits or grants source/publication authority to the candidate.
"""
from __future__ import annotations
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import json
import tomllib


def verify_packages(manifest: Path, kind: str, root: Path, offline: bool) -> None:
    command = ["cargo", "metadata", "--locked", "--format-version", "1", "--manifest-path", str(manifest)]
    if kind == "rows":
        command += ["--no-default-features", "--features", "peerbrush-svg-resvg-tests/text"]
    if offline:
        command.append("--offline")
    metadata = json.loads(subprocess.check_output(command, cwd=root))
    provenance = json.loads((root / "vendor" / "renderer-provenance.json").read_text())
    lock = tomllib.loads(manifest.with_name("Cargo.lock").read_text())
    for expected in provenance["packages"]:
        found = [package for package in metadata["packages"] if package["name"] == expected["name"]]
        if len(found) != 1 or found[0]["version"] != expected["version"]:
            raise ValueError("Renderer oracle/candidate package version differs")
        package = found[0]
        if kind == "oracle":
            if package["source"] != "registry+https://github.com/rust-lang/crates.io-index":
                raise ValueError("The independent oracle must use unmodified registry packages")
            bound = [item for item in lock["package"] if item["name"] == expected["name"] and item["version"] == expected["version"]]
            if len(bound) != 1 or bound[0].get("checksum") != expected["registry_checksum"]:
                raise ValueError("The independent oracle registry checksum differs")
        elif package["source"] is not None or Path(package["manifest_path"]).resolve() != (root / "vendor" / expected["name"] / "Cargo.toml").resolve():
            raise ValueError("The candidate must use only the reviewed pinned local forks")
    if kind == "rows":
        members = [package for package in metadata["packages"] if package["name"] == "peerbrush-svg-resvg-tests"]
        if len(members) != 1 or members[0]["id"] not in metadata["workspace_members"]:
            raise ValueError("The actual resvg unit-test target must be a workspace member")
        member = members[0]
        libraries = [target for target in member["targets"] if "lib" in target["kind"]]
        if len(libraries) != 1 or Path(libraries[0]["src_path"]).resolve() != (root / "vendor/resvg/src/lib.rs").resolve() or not libraries[0]["test"]:
            raise ValueError("Resvg unit tests must compile the unchanged actual vendor library source")
        renderer = next(package for package in metadata["packages"] if package["name"] == "resvg")
        nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
        candidate, tests = nodes[renderer["id"]], nodes[member["id"]]
        if set(candidate["features"]) != {"text"} or set(tests["features"]) != {"text"}:
            raise ValueError("Resvg library tests must retain the candidate's exact text-only features")
        dependencies = {dependency["name"]: dependency["pkg"] for dependency in candidate["deps"]}
        tested = {dependency["name"]: dependency["pkg"] for dependency in tests["deps"]}
        dev = {dependency["name"].replace("-", "_") for dependency in renderer["dependencies"] if dependency["kind"] == "dev"}
        if set(tested) != set(dependencies) | dev or any(tested[name] != package for name, package in dependencies.items()):
            raise ValueError("Resvg unit tests must use the candidate's exact dependency packages plus its dev dependencies")
        required = {dependency["name"]: dependency["req"] for dependency in renderer["dependencies"] if dependency["kind"] == "dev"}
        declared = {dependency["name"]: dependency["req"] for dependency in member["dependencies"] if dependency["kind"] == "dev"}
        if declared != required:
            raise ValueError("Resvg unit tests must retain the actual upstream dev-dependency requirements")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--metadata-only", action="store_true", help="verify exact package separation without building or executing Rust")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if args.metadata_only:
        for kind in ("oracle", "rows"):
            verify_packages(root / "tests" / ("svg-origin-" + kind) / "Cargo.toml", kind, root, args.offline)
        print("Exact registry oracle and pinned candidate package identities verified; no Rust runtime executed")
        return
    with tempfile.TemporaryDirectory(prefix="peerbrush-svg-rows-") as directory:
        work = Path(directory)
        outputs = []
        for kind in ("oracle", "rows"):
            manifest = root / "tests" / ("svg-origin-" + kind) / "Cargo.toml"
            verify_packages(manifest, kind, root, args.offline)
            target = work / ("target-" + kind)
            command = ["cargo", "build", "--release", "--locked", "--bin", "peerbrush-svg-origin-" + kind, "--manifest-path", str(manifest), "--target-dir", str(target)]
            if args.offline:
                command.append("--offline")
            subprocess.run(command, cwd=root, check=True)
            executable = target / "release" / ("peerbrush-svg-origin-" + kind + (".exe" if os.name == "nt" else ""))
            output = work / (kind + ".rgba")
            with output.open("wb") as stream:
                subprocess.run([str(executable)], cwd=root, stdout=stream, check=True)
            outputs.append(output)
        expected, actual = (path.read_bytes() for path in outputs)
        if expected != actual:
            common = min(len(expected), len(actual))
            at = next((n for n in range(common) if expected[n] != actual[n]), common)
            raise SystemExit(f"SVG row mismatch at byte{at}: oracle length{len(expected)}, row length{len(actual)}")
        print(f"Exact registry/row primitive parity: {len(expected)} bytes, SHA256 {hashlib.sha256(expected).hexdigest()}")
        manifest = root / "tests" / "svg-origin-rows" / "Cargo.toml"
        for package in ("tiny-skia-path", "tiny-skia", "resvg"):
            # The workspace member's lib.path is the actual vendor/resvg source;
            # this retains every real dev-dependency unit test. Merely selecting
            # resvg as a nonmember dependency makes Cargo refuse those tests.
            selected = "peerbrush-svg-resvg-tests" if package == "resvg" else package
            command = ["cargo", "test", "--release", "--locked", "--lib", "--package", selected, "--manifest-path", str(manifest), "--target-dir", str(work / "target-rows")]
            if package == "resvg":
                command += ["--no-default-features", "--features", "peerbrush-svg-resvg-tests/text"]
            if args.offline:
                command.append("--offline")
            subprocess.run(command, cwd=root, check=True)
        # The private-unwind adapter is unavailable under panic=abort. Verify
        # that it refuses before callbacks, allocation or entering the kernel.
        environment = os.environ.copy()
        environment["RUSTFLAGS"] = environment.get("RUSTFLAGS", "") + " -Cpanic=abort"
        manifest = root / "tests" / "svg-origin-rows" / "Cargo.toml"
        target = work / "target-abort"
        command = ["cargo", "build", "--release", "--locked", "--bin", "abort_profile", "--manifest-path", str(manifest), "--target-dir", str(target)]
        if args.offline:
            command.append("--offline")
        subprocess.run(command, cwd=root, env=environment, check=True)
        executable = target / "release" / ("abort_profile.exe" if os.name == "nt" else "abort_profile")
        subprocess.run([str(executable)], cwd=root, check=True)


if __name__ == "__main__":
    main()
