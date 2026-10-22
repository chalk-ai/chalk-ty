use super::CursorTest;

fn assert_definition(source: &str, expected_declaration: &str, unexpected_declaration: &str) {
    let test = CursorTest::builder().source("main.py", source).build();
    let definition = test.goto_definition();
    assert!(
        definition.contains(expected_declaration),
        "expected definition to contain {expected_declaration:?}, got:\n{definition}"
    );
    assert!(
        !definition.contains(unexpected_declaration),
        "definition unexpectedly contained {unexpected_declaration:?}:\n{definition}"
    );
}

#[test]
fn explicit_and_contextual_feature_paths_navigate_to_declaration() {
    assert_definition(
        r#"
from chalk.features import _, features

@features
class User:
    email: str
    email_backup: bytes

User.<CURSOR>email
"#,
        "email: str",
        "email_backup: bytes",
    );

    assert_definition(
        r#"
from chalk.features import _ as feature, features

@features
class User:
    email: str
    email_backup: bytes

    feature.<CURSOR>email
"#,
        "email: str",
        "email_backup: bytes",
    );
}

#[test]
fn relationship_feature_path_navigates_to_row_declaration() {
    assert_definition(
        r#"
from chalk.features import DataFrame, _, features

@features
class Transaction:
    amount: str

@features
class User:
    amount: int
    transactions: DataFrame[Transaction]

    _.transactions[_.<CURSOR>amount]
"#,
        "amount: str",
        "amount: int",
    );
}

#[test]
fn imported_feature_class_alias_navigates_to_original_declaration() {
    let test = CursorTest::builder()
        .source(
            "models.py",
            r#"
from chalk.features import features

@features
class User:
    email: str
"#,
        )
        .source(
            "main.py",
            r#"
from models import User as Account

Account.<CURSOR>email
"#,
        )
        .build();

    let definition = test.goto_definition();
    assert!(
        definition.contains("--> models.py:") && definition.contains("email: str"),
        "unexpected definition:\n{definition}"
    );
}

#[test]
fn inherited_feature_path_navigates_to_base_declaration() {
    assert_definition(
        r#"
from chalk.features import features

@features
class BaseUser:
    email: str

@features
class User(BaseUser):
    email_backup: bytes

User.<CURSOR>email
"#,
        "email: str",
        "email_backup: bytes",
    );
}
