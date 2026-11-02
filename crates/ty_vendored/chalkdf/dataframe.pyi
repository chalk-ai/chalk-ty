# BEGIN GENERATED CHALK DATAFRAME IMPORTS
from typing import Any, Generic, TypeVar

from typing_extensions import TypeVarTuple, Unpack

_Columns = TypeVarTuple("_Columns")
_Row = TypeVar("_Row")
_Selection = TypeVar("_Selection")
# END GENERATED CHALK DATAFRAME IMPORTS

class DataFrame(Generic[Unpack[_Columns]]):
    def __getattr__(self, name: str) -> Any: ...
    def __getitem__(self, key: Any) -> Any: ...

# BEGIN GENERATED CHALK PROJECTED DATAFRAME
class ProjectedDataFrame(DataFrame[_Row], Generic[_Row, _Selection]): ...

# END GENERATED CHALK PROJECTED DATAFRAME
