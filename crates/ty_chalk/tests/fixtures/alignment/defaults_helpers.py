from __future__ import annotations

BUDGET = 8


def named_budget(text: str, budget: int = BUDGET) -> int:
    return min(len(text), budget)


def literal_budget(text: str, budget: int = 8) -> int:
    return min(len(text), budget)


def required_budget(text: str, budget: int) -> int:
    return min(len(text), budget)


def global_in_body(text: str) -> int:
    return min(len(text), BUDGET)


def keyword_only_budget(text: str, *, budget: int = BUDGET) -> int:
    return min(len(text), budget)


def positional_only_budget(text: str, budget: int = BUDGET, /) -> int:
    return min(len(text), budget)


def expression_budget(text: str, budget: int = 2 * 4) -> int:
    return min(len(text), budget)


def intermediate(text: str) -> int:
    return named_budget(text)


def unused_helper(text: str, budget: int = BUDGET) -> int:
    return min(len(text), budget)


def literal_positive_int(value: int = 8) -> int:
    return value


def literal_negative_int(value: int = -8) -> int:
    return value


def literal_unary_plus(value: int = +8) -> int:
    return value


def literal_float(value: float = 1.5) -> float:
    return value


def literal_string(value: str = "text") -> str:
    return value


def literal_bool(value: bool = True) -> bool:
    return value


def literal_none(value: None = None) -> None:
    return value
