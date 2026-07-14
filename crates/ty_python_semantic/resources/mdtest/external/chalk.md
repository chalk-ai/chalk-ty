# Chalk

## Symbolic feature paths

`chalk/reexports.pyi`:

```py
from chalk.features import _ as feature
```

`main.py`:

```py
from datetime import datetime

from chalk.features import DataFrame, Primary, Underscore, _, features, has_many
from chalk.features import _ as feature
from chalk.reexports import feature as reexported_feature
from chalk.streams import Windowed
import chalk.functions as F

@features
class Account:
    identifier: str

@features
class Base:
    inherited: int

@features
class Event:
    standardized_email: str
    standardized_national_id: bytes

@features
class User(Base):
    email: str
    primary_email: Primary[str]
    optional_email: str | None
    account: Account
    emails_as_set: Windowed[list[str] | None]
    HAS_ITIN: Windowed[int]
    HAS_SSN_MATCH_DOB: Windowed[int]
    FLOAT_VALUE: Windowed[float]
    chalk_window: str
    records: DataFrame[Event] = has_many(lambda: True)

    direct: str = _.email
    primary_alias: Primary[str] = _.email
    optional: str | None = _.optional_email
    widened_optional: str | None = _.email
    chained: str = _.account.identifier
    inherited_alias: int = _.inherited
    imported_alias: str = feature.email
    reexported_alias: str = reexported_feature.email
    forward_alias: bytes = _.declared_later
    declared_later: bytes
    email_set_365d: list[str] | None = _.emails_as_set["365d"]
    combined_count: int | None = _.HAS_ITIN["all"] + _.HAS_SSN_MATCH_DOB["all"]
    incremented_count: int | None = _.HAS_ITIN["all"] + 1
    reverse_incremented_count: int | None = 1 + _.HAS_ITIN["all"]
    subtracted_count: int | None = _.HAS_ITIN["all"] - _.HAS_SSN_MATCH_DOB["all"]
    multiplied_count: int | None = _.HAS_ITIN["all"] * _.HAS_SSN_MATCH_DOB["all"]
    multiplied_float: float | None = _.HAS_ITIN["all"] * _.FLOAT_VALUE["all"]
    divided_count: float | None = _.HAS_ITIN["all"] / _.HAS_SSN_MATCH_DOB["all"]
    floor_divided_count: int | None = _.HAS_ITIN["all"] // _.HAS_SSN_MATCH_DOB["all"]
    floor_divided_float: float | None = _.HAS_ITIN["all"] // _.FLOAT_VALUE["all"]
    conditional_method: int | None = _.if_then_else(
        _.HAS_ITIN["all"] > 0, _.HAS_ITIN["all"], 0
    )
    conditional_function: str | None = F.if_then_else(
        _.HAS_ITIN["all"] > 0, _.email, None
    )
    nested_conditional: int | None = F.if_then_else(
        _.HAS_ITIN["all"] > 0,
        F.if_then_else(_.HAS_SSN_MATCH_DOB["all"] > 0, 1, 0),
        None,
    )
    combined_match: bool | None = (_.HAS_ITIN["all"] > 0) & (_.HAS_SSN_MATCH_DOB["all"] > 0)
    normal_underscore_member: datetime = _.chalk_window
    record_email: str = _.standardized_email

    bad: int = _.email  # error: [invalid-assignment] "Object of type `str` is not assignable to `int`"
    missing: str = _.does_not_exist  # error: [unresolved-attribute]

    reveal_type(_.email)  # revealed: Resolved[str]
    reveal_type(_.primary_email)  # revealed: Resolved[str]
    reveal_type(_.chalk_window)  # revealed: datetime
    reveal_type(_.standardized_national_id)  # revealed: Resolved[bytes]
    reveal_type(_.HAS_ITIN["all"] + _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] + 1)  # revealed: Resolved[int]
    reveal_type(1 + _.HAS_ITIN["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] - _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(1 - _.HAS_ITIN["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] * _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(2.0 * _.HAS_ITIN["all"])  # revealed: Resolved[float]
    reveal_type(_.HAS_ITIN["all"] * _.FLOAT_VALUE["all"])  # revealed: Resolved[float]
    reveal_type(_.HAS_ITIN["all"] / _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[float]
    reveal_type(1 / _.HAS_ITIN["all"])  # revealed: Resolved[float]
    reveal_type(_.HAS_ITIN["all"] // _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(1.0 // _.HAS_ITIN["all"])  # revealed: Resolved[float]
    reveal_type(_.HAS_ITIN["all"] // _.FLOAT_VALUE["all"])  # revealed: Resolved[float]
    reveal_type(_.if_then_else(_.HAS_ITIN["all"] > 0, _.HAS_ITIN["all"], 0))  # revealed: Resolved[int]
    reveal_type(F.if_then_else(_.HAS_ITIN["all"] > 0, _.email, None))  # revealed: Resolved[str | None]
    reveal_type(F.if_then_else(_.HAS_ITIN["all"] > 0, F.if_then_else(_.HAS_SSN_MATCH_DOB["all"] > 0, 1, 0), None))  # revealed: Resolved[Literal[1, 0] | None]
    reveal_type(_.HAS_ITIN["all"] > 0)  # revealed: Resolved[bool]
    reveal_type((_.HAS_ITIN["all"] > 0) & (_.HAS_SSN_MATCH_DOB["all"] > 0))  # revealed: Resolved[bool]
    reveal_type(True & (_.HAS_ITIN["all"] > 0))  # revealed: Resolved[bool]
    reveal_type(False | (_.HAS_ITIN["all"] > 0))  # revealed: Resolved[bool]
    reveal_type(True ^ (_.HAS_ITIN["all"] > 0))  # revealed: Resolved[bool]
    reveal_type(direct)  # revealed: Resolved[str]
    reveal_type(primary_alias)  # revealed: Resolved[str]

reveal_type(User.email)  # revealed: Resolved[str]
reveal_type(User.primary_email)  # revealed: Resolved[str]
reveal_type(User.inherited)  # revealed: Resolved[int]
reveal_type(User.account.identifier)  # revealed: Resolved[str]
reveal_type(User().email)  # revealed: str
reveal_type(User().primary_email)  # revealed: str
reveal_type(User().optional_email)  # revealed: str | None
reveal_type(User(primary_email="primary@example.com").primary_email)  # revealed: str
reveal_type(User.emails_as_set)  # revealed: Windowed[list[str] | None]
reveal_type(User.chalk_window)  # revealed: Resolved[str]

def accepts_underscore(value: Underscore) -> None: ...

accepts_underscore(User.email)
reveal_type(User.emails_as_set["365d"])  # revealed: Resolved[list[str] | None]

def resolver(email: User.email, primary_email: User.primary_email, identifier: Account.identifier) -> None:
    reveal_type(email)  # revealed: str
    reveal_type(primary_email)  # revealed: str
    reveal_type(identifier)  # revealed: str

not_a_feature_value: str = User.email  # error: [invalid-assignment]

class Shadow:
    email: str

@features
class LocallyShadowed:
    _: Shadow = Shadow()
    alias: str = _.email

    reveal_type(_.email)  # revealed: str

_ = Shadow()
reveal_type(_.email)  # revealed: str
```

The Chalk underscore remains an ordinary runtime placeholder outside a feature class:

```py
from chalk.features import _

reveal_type(_.anything)  # revealed: Any
```
