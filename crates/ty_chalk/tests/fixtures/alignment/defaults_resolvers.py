from __future__ import annotations

from chalk import online
from defaults_helpers import (
    expression_budget,
    global_in_body,
    intermediate,
    keyword_only_budget,
    literal_bool,
    literal_budget,
    literal_float,
    literal_negative_int,
    literal_none,
    literal_positive_int,
    literal_string,
    literal_unary_plus,
    named_budget,
    positional_only_budget,
    required_budget,
)


@online
def omitted_named_default(text: str) -> int:
    return named_budget(text)


@online
def overridden_by_keyword(text: str) -> int:
    return named_budget(text, budget=3)


@online
def overridden_positionally(text: str) -> int:
    return named_budget(text, 3)


@online
def omitted_keyword_only_default(text: str) -> int:
    return keyword_only_budget(text)


@online
def omitted_positional_only_default(text: str) -> int:
    return positional_only_budget(text)


@online
def constant_expression_default(text: str) -> int:
    return expression_budget(text)


@online
def transitive_helper_call(text: str) -> int:
    return intermediate(text)


# Supported controls: these should not receive a default-related warning.


@online
def literal_default_control(text: str) -> int:
    return literal_budget(text)


@online
def explicit_required_argument_control(text: str) -> int:
    return required_budget(text, budget=3)


@online
def ordinary_global_control(text: str) -> int:
    return global_in_body(text)


@online
def literal_positive_int_control() -> int:
    return literal_positive_int()


@online
def literal_negative_int_control() -> int:
    return literal_negative_int()


@online
def literal_unary_plus_control() -> int:
    return literal_unary_plus()


@online
def literal_float_control() -> float:
    return literal_float()


@online
def literal_string_control() -> str:
    return literal_string()


@online
def literal_bool_control() -> bool:
    return literal_bool()


@online
def literal_none_control() -> None:
    return literal_none()
