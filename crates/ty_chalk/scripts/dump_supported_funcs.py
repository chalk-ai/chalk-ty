#!/usr/bin/env python3
"""Print static accelerator signatures in the vendored registry format."""

from static_accelerator import signatures

for supported_call, implementations in signatures.SUPPORTED_FUNCS.items():
    print(
        (
            supported_call,
            [implementation.signature for implementation in implementations],
        )
    )
