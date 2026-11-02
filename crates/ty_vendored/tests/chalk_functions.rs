use std::collections::HashSet;

#[test]
fn public_chalk_functions_match_generated_stub() {
    let precise_stub = include_str!("../../ty_chalk/scripts/chalk_functions_precise.pyi");
    let stub = include_str!("../chalk/functions/__init__.pyi");
    let precise = precise_stub
        .lines()
        .filter_map(|line| line.strip_prefix("def "))
        .map(|declaration| {
            declaration
                .split_once('(')
                .expect("precise declaration to be a function")
                .0
        })
        .collect::<HashSet<_>>();
    let generated = include_str!("../chalk/functions/public_functions.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|name| !precise.contains(name))
        .map(|name| format!("def {name}(*args: Any, **kwargs: Any) -> Any: ..."))
        .collect::<Vec<_>>()
        .join("\n");
    let expected = format!(
        "{precise_stub}\n# BEGIN GENERATED CHALK FUNCTIONS\n{generated}\n\n# END GENERATED CHALK FUNCTIONS\n\ndef __getattr__(name: str) -> Any: ...\n"
    );

    assert_eq!(stub, expected);
}
