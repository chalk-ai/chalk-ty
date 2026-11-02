#!/usr/bin/env python3
"""Refresh vendored Chalk stubs and the function manifest from chalkpy.

The selected Python interpreter must have the target chalkpy version installed.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.ArgumentDefaultsHelpFormatter
    )
    parser.add_argument(
        "--python",
        type=Path,
        default=Path(sys.executable),
        help="Python interpreter with the target chalkpy version installed",
    )
    args = parser.parse_args()

    scripts = Path(__file__).resolve().parent
    vendored = scripts.parents[1] / "ty_vendored"
    functions = vendored / "chalk" / "functions"
    manifest = functions / "public_functions.txt"

    with tempfile.TemporaryDirectory() as temporary_directory:
        temporary = Path(temporary_directory)
        generated_manifest = temporary / manifest.name
        generated_vendored = temporary / "ty_vendored"

        with generated_manifest.open("w") as output:
            subprocess.run(
                [args.python, scripts / "dump_chalk_functions.py"],
                stdout=output,
                check=True,
            )
        for relative_path in (
            "chalk/features/underscore.pyi",
            "chalkdf/dataframe.pyi",
            "ty_chalk_extensions/__init__.pyi",
        ):
            source = vendored / relative_path
            destination = generated_vendored / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        subprocess.run(
            [
                sys.executable,
                scripts / "generate_chalk_functions_stub.py",
                generated_manifest,
                "--vendored-root",
                generated_vendored,
            ],
            check=True,
        )

        shutil.copyfile(generated_manifest, manifest)
        for generated_stub in generated_vendored.rglob("*.pyi"):
            destination = vendored / generated_stub.relative_to(generated_vendored)
            shutil.copyfile(generated_stub, destination)

    print(f"Updated {manifest.relative_to(vendored)} and Chalk stubs")


if __name__ == "__main__":
    main()
