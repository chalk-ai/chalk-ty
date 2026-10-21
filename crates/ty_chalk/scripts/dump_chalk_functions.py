#!/usr/bin/env python3
"""Print the public functions exported by the installed chalkpy package."""

from __future__ import annotations

import importlib.metadata
import inspect


def main() -> None:
    import chalk.functions as functions

    names = sorted(
        name
        for name in functions.__all__
        if inspect.isfunction(getattr(functions, name))
    )
    version = importlib.metadata.version("chalkpy")

    print(f"# Generated from chalkpy {version} chalk.functions.__all__.")
    print(*names, sep="\n")


if __name__ == "__main__":
    main()
