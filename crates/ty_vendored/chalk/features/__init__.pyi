from typing import Any, Callable, Generic, TypeVar

from chalkdf.dataframe import DataFrame as DataFrame

from .feature_set_decorator import features as features
from .primary import Primary as Primary
from .underscore import (
    Underscore as Underscore,
    UnderscoreRoot as UnderscoreRoot,
    _ as _,
)

_T = TypeVar("_T")

class Features(Generic[_T]): ...

def has_many(join: Callable[[], Any]) -> Any: ...
def __getattr__(name: str) -> Any: ...
