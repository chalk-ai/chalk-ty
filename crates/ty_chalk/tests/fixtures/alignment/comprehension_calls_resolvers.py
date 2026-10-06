from __future__ import annotations

import math

from chalk import online

BUDGET = 8


def named_default(value: int, limit: int = BUDGET) -> int:
    return value


def unsupported_body(value: int) -> int:
    return math.gcd(value, 2)


def identity(value: int) -> int:
    return value


def iterable_default(values: list[int], limit: int = BUDGET) -> list[int]:
    return values


@online
def outside_call_control(values: list[int]):
    return math.gcd(values[0], 2)


@online
def list_element_call(values: list[int]):
    return [math.gcd(value, 2) for value in values]


@online
def list_filter_call(values: list[int]):
    return [value for value in values if math.gcd(value, 2)]


@online
def list_iterable_call(values: list[int]):
    return [value for value in iterable_default(values)]  # noqa: C416 -- Probe iterable calls.


@online
def list_nested_iterable_call(values: list[int]):
    return [other for value in values for other in iterable_default(values)]


@online
def list_named_default(values: list[int]):
    return [named_default(value) for value in values]


@online
def list_helper_body(values: list[int]):
    return [unsupported_body(value) for value in values]


@online
def list_supported_helper(values: list[int]):
    return [identity(value) for value in values]


@online
def list_supported_builtin(values: list[int]):
    return [abs(value) for value in values]


@online
def list_shadowed_builtin(values: list[int]):
    return [abs(-1) for abs in [identity]]


@online
def list_empty_input(values: list[int]):
    return [math.gcd(value, 2) for value in []]


@online
def list_false_filter(values: list[int]):
    return [math.gcd(value, 2) for value in values if False]


@online
def set_element_call(values: list[int]):
    return {math.gcd(value, 2) for value in values}


@online
def set_literal_control(values: list[int]):
    return {value for value in values}  # noqa: C416 -- Probe set-comprehension support.


@online
def generator_consumed(values: list[int]):
    return sum(math.gcd(value, 2) for value in values)


@online
def generator_control(values: list[int]):
    return sum(value for value in values)


@online
def generator_unused(values: list[int]):
    _result = (math.gcd(value, 2) for value in values)
    return 1


@online
def generator_unused_control(values: list[int]):
    _result = (value for value in values)
    return 1
