"""Compile and execute the small pinned sharper-filter differential fixture."""
from pathlib import Path
import platform
import subprocess
import tempfile

from jxl_build import configure


def main():
    root = Path(__file__).resolve().parents[1]
    environment, _ = configure()
    with tempfile.TemporaryDirectory(prefix="peerbrush-sharper-rows-") as temporary:
        output = Path(temporary) / ("filter.exe" if platform.system() == "Windows" else "filter")
        source = root / "tests/jxl_sharper_rows.cpp"
        includes = root / "vendor/jpegxl-src/libjxl"
        if platform.system() == "Windows":
            command = [environment["CXX"], "/nologo", "/std:c++17", "/O2", "/EHsc",
                       "/I" + str(includes), str(source), "/Fo" + str(output.with_suffix(".obj")),
                       "/Fe" + str(output)]
        else:
            command = [environment["CXX"], "-std=c++17", "-O2", "-I", str(includes),
                       str(source), "-o", str(output)]
        subprocess.run(command, cwd=root, env=environment, check=True, timeout=120)
        # No file-data decoder or app entry: fixed scalar fixtures below 1 MiB.
        subprocess.run([str(output)], cwd=root, env=environment, check=True, timeout=15)


if __name__ == "__main__":
    main()
