#!/usr/bin/env python3
"""Print static accelerator signatures in the vendored registry format."""

from __future__ import annotations

import importlib
import inspect
import sys
from collections.abc import Iterable
from types import ModuleType
from typing import Any


def resolve_path(path: str) -> Any:
    """Resolve a dotted path to the Python object exported at that path."""
    components = path.split(".")
    for module_end in range(len(components), 0, -1):
        module_name = ".".join(components[:module_end])
        try:
            value = importlib.import_module(module_name)
        except ModuleNotFoundError as error:
            if error.name != module_name:
                raise
            continue
        for attribute in components[module_end:]:
            value = getattr(value, attribute)
        return value
    raise ModuleNotFoundError(path)


def native_receiver_path(path: str) -> str:
    """Return the module or class identity used by ty for a receiver expression."""
    value = resolve_path(path)
    if isinstance(value, ModuleType):
        return value.__name__
    cls = value if inspect.isclass(value) else type(value)
    return f"{cls.__module__}.{cls.__qualname__}"


def normalized_signature_repr(
    signature: Any, *, has_receiver: bool
) -> tuple[str, tuple[str, str] | None]:
    """Render a signature with its module-like receiver's native identity."""
    if not has_receiver or not signature.args:
        return repr(signature), None
    receiver = signature.args[0].ty
    if type(receiver).__name__ != "TyModule":
        return repr(signature), None

    registry_path = receiver.name
    native_path = native_receiver_path(registry_path)
    if registry_path == native_path:
        return repr(signature), None

    rendered_receiver = repr(receiver)
    rendered_native_receiver = rendered_receiver.replace(
        f"name={registry_path!r}", f"name={native_path!r}", 1
    )
    if rendered_native_receiver == rendered_receiver:
        raise ValueError(f"cannot replace receiver path in {rendered_receiver}")
    return (
        repr(signature).replace(rendered_receiver, rendered_native_receiver, 1),
        (registry_path, native_path),
    )


def dump_supported_funcs(
    supported_funcs: dict[Any, Iterable[Any]],
) -> set[tuple[str, str]]:
    normalizations = set()
    for supported_call, implementations in supported_funcs.items():
        has_receiver = type(supported_call).__name__ in {
            "SupportedAttribute",
            "SupportedMethod",
        }
        rendered_signatures = []
        for implementation in implementations:
            rendered, normalization = normalized_signature_repr(
                implementation.signature, has_receiver=has_receiver
            )
            rendered_signatures.append(rendered)
            if normalization is not None:
                normalizations.add(normalization)
        print(f"({supported_call!r}, [{', '.join(rendered_signatures)}])")
    return normalizations


def main() -> None:
    from static_accelerator import signatures

    normalizations = dump_supported_funcs(signatures.SUPPORTED_FUNCS)
    for registry_path, native_path in sorted(normalizations):
        print(
            f"Normalized registry receiver {registry_path!r} to {native_path!r}",
            file=sys.stderr,
        )


if __name__ == "__main__":
    main()
