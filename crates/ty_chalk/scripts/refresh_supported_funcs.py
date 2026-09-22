#!/usr/bin/env python3
"""Refresh ty_chalk's supported-function registry from static_accelerator."""

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
        "static_accelerator",
        nargs="?",
        type=Path,
        default=Path("~/Development/chalk/chalk-private/static_accelerator"),
        help="path to the static_accelerator project in a chalk-private checkout",
    )
    args = parser.parse_args()

    static_accelerator = args.static_accelerator.expanduser().resolve()
    signatures = (
        static_accelerator / "static_accelerator" / "signatures" / "__init__.py"
    )
    if (
        not signatures.is_file()
        or not (static_accelerator / "pyproject.toml").is_file()
    ):
        parser.error(f"not a static_accelerator project: {static_accelerator}")

    scripts = Path(__file__).resolve().parent
    crate = scripts.parent
    workspace = crate.parents[1]
    registry = scripts / "SUPPORTED_FUNCS.data"
    snapshot = crate / "src" / "supported_functions" / "current_snapshot.rs"

    with tempfile.TemporaryDirectory() as temporary_directory:
        temporary = Path(temporary_directory)
        generated_registry = temporary / registry.name
        generated_snapshot = temporary / snapshot.name

        with generated_registry.open("w") as output:
            subprocess.run(
                ["uv", "run", scripts / "dump_supported_funcs.py"],
                cwd=static_accelerator,
                stdout=output,
                check=True,
            )
        subprocess.run(
            [
                sys.executable,
                scripts / "generate_supported_funcs.py",
                generated_registry,
                "-o",
                generated_snapshot,
            ],
            check=True,
        )
        subprocess.run(
            [
                "rustfmt",
                "--edition",
                "2024",
                "--config-path",
                workspace,
                generated_snapshot,
            ],
            check=True,
        )

        shutil.copyfile(generated_registry, registry)
        shutil.copyfile(generated_snapshot, snapshot)

    print(f"Updated {registry.relative_to(crate)} and {snapshot.relative_to(crate)}")


if __name__ == "__main__":
    main()
