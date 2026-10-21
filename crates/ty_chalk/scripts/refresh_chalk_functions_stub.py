#!/usr/bin/env python3
"""Refresh the vendored chalk.functions manifest and stub from chalkpy.

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
    stub = functions / "__init__.pyi"

    with tempfile.TemporaryDirectory() as temporary_directory:
        temporary = Path(temporary_directory)
        generated_manifest = temporary / manifest.name
        generated_stub = temporary / stub.name

        with generated_manifest.open("w") as output:
            subprocess.run(
                [args.python, scripts / "dump_chalk_functions.py"],
                stdout=output,
                check=True,
            )
        subprocess.run(
            [
                sys.executable,
                scripts / "generate_chalk_functions_stub.py",
                generated_manifest,
                stub,
                "-o",
                generated_stub,
            ],
            check=True,
        )

        shutil.copyfile(generated_manifest, manifest)
        shutil.copyfile(generated_stub, stub)

    print(f"Updated {manifest.relative_to(vendored)} and {stub.relative_to(vendored)}")


if __name__ == "__main__":
    main()
