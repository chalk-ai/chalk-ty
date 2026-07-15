from typing import Any

from .dataframe import DataFrame as DataFrame

def __getattr__(name: str) -> Any: ...
