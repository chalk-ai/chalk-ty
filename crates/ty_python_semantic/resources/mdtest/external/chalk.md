# Chalk

## Symbolic feature paths

`chalk/reexports.pyi`:

```pyi
from chalk.features import _

feature = _
```

`main.py`:

```py
from datetime import datetime

from chalk.features import DataFrame, Primary, Underscore, _, features, has_many
from chalk.features import _ as feature
from chalk.reexports import feature as reexported_feature
from chalk.streams import Windowed
from chalkdf import DataFrame as ChalkDataFrame
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
    conditional_method: int | None = _.if_then_else(_.HAS_ITIN["all"] > 0, _.HAS_ITIN["all"], 0)
    conditional_function: str | None = F.if_then_else(_.HAS_ITIN["all"] > 0, _.email, None)
    nested_conditional: int | None = F.if_then_else(
        _.HAS_ITIN["all"] > 0,
        F.if_then_else(_.HAS_SSN_MATCH_DOB["all"] > 0, 1, 0),
        None,
    )
    combined_match: bool | None = (_.HAS_ITIN["all"] > 0) & (_.HAS_SSN_MATCH_DOB["all"] > 0)
    normal_underscore_member: datetime = _.chalk_window
    relationship_member_without_receiver: str = _.standardized_email  # error: [unresolved-attribute]
    bad: int = _.email  # error: [invalid-assignment] "Object of type `str` is not assignable to `int`"
    missing: str = _.does_not_exist  # error: [unresolved-attribute]

    reveal_type(_.email)  # revealed: Resolved[str]
    reveal_type(_.primary_email)  # revealed: Resolved[str]
    reveal_type(_.chalk_window)  # revealed: datetime
    reveal_type(_.HAS_ITIN["all"] + _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] + 1)  # revealed: Resolved[int]
    reveal_type(1 + _.HAS_ITIN["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] - _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(1 - _.HAS_ITIN["all"])  # revealed: Resolved[int]
    reveal_type(_.HAS_ITIN["all"] * _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(2.0 * _.HAS_ITIN["all"])  # revealed: Resolved[int | float]
    reveal_type(_.HAS_ITIN["all"] * _.FLOAT_VALUE["all"])  # revealed: Resolved[int | float]
    reveal_type(_.HAS_ITIN["all"] / _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int | float]
    reveal_type(1 / _.HAS_ITIN["all"])  # revealed: Resolved[int | float]
    reveal_type(_.HAS_ITIN["all"] // _.HAS_SSN_MATCH_DOB["all"])  # revealed: Resolved[int]
    reveal_type(1.0 // _.HAS_ITIN["all"])  # revealed: Resolved[int | float]
    reveal_type(_.HAS_ITIN["all"] // _.FLOAT_VALUE["all"])  # revealed: Resolved[int | float]
    reveal_type(_.if_then_else(_.HAS_ITIN["all"] > 0, _.HAS_ITIN["all"], 0))  # revealed: Resolved[int]
    reveal_type(F.if_then_else(_.HAS_ITIN["all"] > 0, _.email, None))  # revealed: Resolved[str | None]
    reveal_type(nested_conditional)  # revealed: Resolved[Literal[1, 0] | None]
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
reveal_type(User(primary_email="primary@example.com").primary_email)  # revealed: Literal["primary@example.com"]
reveal_type(User.emails_as_set)  # revealed: Windowed[list[str] | None]
reveal_type(User.chalk_window)  # revealed: Resolved[str]

def variadic_dataframe(value: ChalkDataFrame) -> DataFrame[User.email, User.optional_email]:
    return value

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

def local_shadow() -> None:
    _ = Shadow()
    reveal_type(_.email)  # revealed: str
```

The Chalk underscore remains an ordinary runtime placeholder outside a feature class:

```py
from chalk.features import _

reveal_type(_.anything)  # revealed: Any
```

## Feature references in feature annotations

Feature fields can reference other features as their annotations, including forward references.
Feature references remain invalid in variable annotations outside a feature class.

```py
from chalk.features import Primary, features

@features
class Account:
    id: Primary[int]

@features()
class Transaction:
    account_id: Account.id
    user_id: "User.id"

@features()
class User:
    id: Primary[int]

reveal_type(Transaction.account_id)  # revealed: Resolved[int]
reveal_type(Transaction().account_id)  # revealed: int
reveal_type(Transaction.user_id)  # revealed: Resolved[int]
reveal_type(Transaction().user_id)  # revealed: int

module_value: User.id  # error: [invalid-type-form]
```

## Receiver-scoped relationship subscripts

Within a Chalk relationship subscript, `_` resolves against the row type addressed by the complete
receiver path. This applies to underscore-rooted paths inside feature classes and explicit
feature-rooted paths outside them. A member on an unrelated relationship cannot satisfy the lookup.

```py
from datetime import datetime

from chalk.features import DataFrame, _, features, has_many

@features
class Event:
    timestamp: datetime

@features
class Purchase:
    amount: str

@features
class User:
    id: int
    bare_events: DataFrame[Event]
    events: DataFrame[Event] = has_many(lambda: True)
    purchases: DataFrame[Purchase] = has_many(lambda: True)

    _.bare_events[_.timestamp]
    _.events[_.timestamp, _.timestamp > _.chalk_window, _.timestamp <= _.chalk_now]
    _.events[_.amount]  # error: [unresolved-attribute]

def events_resolver(events: User.events[_.timestamp]) -> None: ...
```

Explicit feature paths retain the receiver context through optional intermediate relationships and
forward-referenced row types. Nested subscripts replace the context with their immediate receiver.

```py
from chalk.features import DataFrame, _, features

@features
class PaymentAccount:
    charges: "DataFrame[Charge]"

@features
class Account:
    payment_account: PaymentAccount | None

@features
class Driver:
    account: Account

@features
class Charge:
    risk_score: int | None

def charges_resolver(
    charges: Driver.account.payment_account.charges[_.risk_score],
) -> None: ...

Driver.account.payment_account.charges[_.does_not_exist]  # error: [unresolved-attribute]
```

```py
from chalk.features import DataFrame, _, features

@features
class Event:
    timestamp: int

@features
class Group:
    events: DataFrame[Event]

@features
class User:
    groups: DataFrame[Group]

    _.groups[_.events[_.timestamp]]
```
