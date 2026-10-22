use super::{CursorTest, hover_test};

fn assert_feature_hover(source: &str, expected: &str) {
    let hover = hover_test(source).hover();
    assert!(
        hover.starts_with(expected),
        "expected hover to start with {expected:?}, got:\n{hover}"
    );
}

#[test]
fn explicit_and_contextual_feature_paths_show_owner_and_declared_type() {
    assert_feature_hover(
        r#"
from chalk.features import _, features

@features
class User:
    email: str

User.<CURSOR>email
"#,
        "(feature) User.email: str",
    );

    assert_feature_hover(
        r#"
from chalk.features import _ as feature, features

@features
class User:
    email: str

    feature.<CURSOR>email
"#,
        "(feature) User.email: str",
    );
}

#[test]
fn relationship_feature_path_uses_row_owner() {
    assert_feature_hover(
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
        "(feature) Transaction.amount: str",
    );
}

#[test]
fn ordinary_feature_instance_access_keeps_standard_hover() {
    let hover = hover_test(
        r#"
from chalk.features import features

@features
class User:
    email: str

def email(user: User):
    return user.<CURSOR>email
"#,
    )
    .hover();

    assert!(hover.starts_with("str"), "unexpected hover:\n{hover}");
}

#[test]
fn inherited_feature_path_shows_declaring_owner() {
    assert_feature_hover(
        r#"
from chalk.features import features

@features
class BaseUser:
    email: str

@features
class User(BaseUser):
    pass

User.<CURSOR>email
"#,
        "(feature) BaseUser.email: str",
    );
}

#[test]
fn imported_feature_class_alias_shows_original_owner() {
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

    let hover = test.hover();
    assert!(
        hover.starts_with("(feature) User.email: str"),
        "unexpected hover:\n{hover}"
    );
}
