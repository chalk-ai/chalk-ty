# Chalk resolver types

## Feature-path parameters

Feature paths in resolver annotations have their runtime value types, including primary and nested
features.

```py
from chalk.features import Primary, features

@features
class Account:
    identifier: str

@features
class User:
    id: int
    primary_id: Primary[int]
    name: str
    account: Account

def resolver(
    id: User.id,
    primary_id: User.primary_id,
    account_id: User.account.identifier,
) -> User.name:
    reveal_type(id)  # revealed: int
    reveal_type(primary_id)  # revealed: int
    reveal_type(account_id)  # revealed: str
    return account_id

def wrong_return(id: User.id) -> User.name:
    return id  # error: [invalid-return-type]
```

## Structured returns

A feature-class constructor satisfies `Features[...]` only for the selected fields that the call
definitely supplies. Missing diagnostics retain nested feature paths.

```py
from chalk.features import Features, features

@features
class Account:
    identifier: str

@features
class User:
    name: str
    score: float
    account: Account

def complete() -> Features[User.name, User.score]:
    return User(name="Ada", score=1.0)

def complete_nested() -> Features[User.name, User.account.identifier]:
    return User(name="Ada", account=Account(identifier="account"))

def extra_fields_are_allowed() -> Features[User.name]:
    return User(name="Ada", score=1.0)

def missing_score() -> Features[User.name, User.score]:
    return User(name="Ada")  # error: [invalid-return-type] "Return value is missing required field `score`"

def missing_multiple() -> Features[User.name, User.score, User.account.identifier]:
    # error: [invalid-return-type] "Return value is missing required fields `account.identifier` and `score`"
    return User(name="Ada")
```

## Scalar returns

`Features[...]` selecting exactly one terminal feature also accepts that feature's scalar runtime
value. Multiple selected features still require a structured return.

```py
from chalk.features import Features, features

@features
class Account:
    identifier: str

@features
class User:
    name: str
    score: float
    account: Account

def scalar() -> Features[User.name]:
    return "Ada"

def nested_scalar() -> Features[User.account.identifier]:
    return "account"

def wrong_scalar() -> Features[User.name]:
    return 1  # error: [invalid-return-type]

def multiple_features() -> Features[User.name, User.score]:
    return "Ada"  # error: [invalid-return-type]
```

## Constructor errors

Invalid supplied feature values retain their constructor diagnostics without adding a redundant
return-type diagnostic.

```py
from chalk.features import Features, features

@features
class User:
    name: str
    score: float

def resolver() -> Features[User.name, User.score]:
    return User(
        name=1,  # error: [invalid-argument-type]
        score="high",  # error: [invalid-argument-type]
    )
```
