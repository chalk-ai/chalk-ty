#[test]
fn public_chalk_functions_match_generated_stub() {
    const PRECISE_DECLARATIONS: &[&str] =
        &["if_then_else", "max_by", "max_by_n", "min_by", "min_by_n"];

    let expected = include_str!("../chalk/functions/public_functions.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|name| !PRECISE_DECLARATIONS.contains(name))
        .collect::<Vec<_>>();
    let stub = include_str!("../chalk/functions/__init__.pyi");
    let (_, generated) = stub
        .split_once("# BEGIN GENERATED CHALK FUNCTIONS")
        .expect("generated Chalk functions block to start");
    let (generated, _) = generated
        .split_once("# END GENERATED CHALK FUNCTIONS")
        .expect("generated Chalk functions block to end");
    let actual = generated
        .lines()
        .filter_map(|line| line.strip_prefix("def "))
        .map(|declaration| {
            declaration
                .split_once('(')
                .expect("generated declaration to be a function")
                .0
        })
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}
