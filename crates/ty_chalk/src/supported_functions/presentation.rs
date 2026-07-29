use std::collections::BTreeSet;

use super::{
    CallKind, SupportedArg, SupportedCall, SupportedFuncs, SupportedSignature, SupportedTy,
};

const DISPLAY_LIMIT: usize = 2;
const MAX_PRESENTATION_WORK: usize = 1 << 16;

#[derive(Debug)]
struct PresentationBudget {
    remaining: usize,
}

impl PresentationBudget {
    const fn new() -> Self {
        Self {
            remaining: MAX_PRESENTATION_WORK,
        }
    }

    fn spend(&mut self, factors: impl IntoIterator<Item = usize>) -> Result<(), BudgetExceeded> {
        let amount = factors
            .into_iter()
            .try_fold(1usize, usize::checked_mul)
            .ok_or(BudgetExceeded)?;
        self.remaining = self.remaining.checked_sub(amount).ok_or(BudgetExceeded)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct BudgetExceeded;

/// Presents the registry signatures for one or more possible source-call targets.
///
/// Displays are deduplicated and sorted across every target. Missing registry entries contribute
/// no display. At most two unique signatures are returned, followed by `... [N more]` when more
/// unique displays exist.
#[must_use]
pub(crate) fn present_supported_signatures(
    supported: &SupportedFuncs,
    calls: &[SupportedCall],
) -> Vec<String> {
    let calls = calls.iter().collect::<BTreeSet<_>>();
    let mut budget = PresentationBudget::new();
    let displays = collapsed_displays(supported, &calls, &mut budget)
        .unwrap_or_else(|_| raw_displays(supported, &calls));
    summarize(displays)
}

fn collapsed_displays(
    supported: &SupportedFuncs,
    calls: &BTreeSet<&SupportedCall>,
    budget: &mut PresentationBudget,
) -> Result<BTreeSet<String>, BudgetExceeded> {
    let mut displays = BTreeSet::new();
    for call in calls.iter().copied() {
        let Some(signatures) = supported.signatures(call.kind(), call.name()) else {
            continue;
        };
        displays.extend(displays_for_call(call, signatures, budget)?);
    }
    Ok(displays)
}

fn raw_displays(supported: &SupportedFuncs, calls: &BTreeSet<&SupportedCall>) -> BTreeSet<String> {
    calls
        .iter()
        .copied()
        .filter_map(|call| {
            supported
                .signatures(call.kind(), call.name())
                .map(|signatures| (call, signatures))
        })
        .flat_map(|(call, signatures)| {
            signatures
                .iter()
                .map(move |signature| format_signature(call, signature))
        })
        .collect()
}

fn summarize(displays: BTreeSet<String>) -> Vec<String> {
    let omitted = displays.len().saturating_sub(DISPLAY_LIMIT);
    let mut summary = displays.into_iter().take(DISPLAY_LIMIT).collect::<Vec<_>>();
    if omitted > 0 {
        summary.push(format!("... [{omitted} more]"));
    }
    summary
}

fn displays_for_call(
    call: &SupportedCall,
    signatures: &[SupportedSignature],
    budget: &mut PresentationBudget,
) -> Result<BTreeSet<String>, BudgetExceeded> {
    let max_arity = signatures
        .iter()
        .map(|signature| presented_args(call.kind(), signature).len())
        .max()
        .unwrap_or_default();
    budget.spend([signatures.len(), signatures.len(), max_arity.max(1)])?;

    let mut groups: Vec<Vec<&SupportedSignature>> = Vec::new();
    for signature in signatures {
        if let Some(group) = groups.iter_mut().find(|group| {
            group
                .first()
                .is_some_and(|first| same_argument_metadata(call.kind(), first, signature))
        }) {
            group.push(signature);
        } else {
            groups.push(vec![signature]);
        }
    }

    let mut displays = BTreeSet::new();
    for group in groups {
        add_group_displays(call, &group, &mut displays, budget)?;
    }
    Ok(displays)
}

fn add_group_displays(
    call: &SupportedCall,
    signatures: &[&SupportedSignature],
    displays: &mut BTreeSet<String>,
    budget: &mut PresentationBudget,
) -> Result<(), BudgetExceeded> {
    let Some(first_signature) = signatures.first() else {
        return Ok(());
    };
    let arity = presented_args(call.kind(), first_signature).len();
    budget.spend([signatures.len(), signatures.len(), arity.max(1)])?;

    let has_non_none_type = (0..arity)
        .map(|index| {
            signatures
                .iter()
                .any(|signature| !is_none(presented_args(call.kind(), signature)[index].ty()))
        })
        .collect::<Vec<_>>();
    let mut shapes = Vec::new();

    for (index, signature) in signatures.iter().enumerate() {
        let args = presented_args(call.kind(), signature);
        if !(0..arity).all(|index| !has_non_none_type[index] || !is_none(args[index].ty())) {
            continue;
        }

        if !shapes.iter().any(|shape_index| {
            same_non_none_shape(call.kind(), signature, signatures[*shape_index])
        }) {
            shapes.push(index);
        }
    }

    budget.spend([shapes.len(), signatures.len(), arity.max(1)])?;
    let mut covered = vec![false; signatures.len()];
    for shape_index in shapes {
        let shape = signatures[shape_index];
        let compatible = signatures
            .iter()
            .enumerate()
            .filter(|(_, signature)| signature_matches_shape(call.kind(), signature, shape))
            .map(|(index, signature)| (index, *signature))
            .collect::<Vec<_>>();
        let shape_args = presented_args(call.kind(), shape);
        let nullable = (0..arity)
            .map(|index| {
                !is_none(shape_args[index].ty())
                    && compatible.iter().any(|(_, signature)| {
                        presented_args(call.kind(), signature)[index]
                            .ty()
                            .accepts_none()
                    })
            })
            .collect::<Vec<_>>();

        if !nullable.iter().any(|&nullable| nullable)
            || !covers_full_cartesian_product(call.kind(), shape, &nullable, &compatible, budget)?
        {
            continue;
        }

        displays.insert(format_collapsed_signature(
            call,
            first_signature,
            shape,
            &nullable,
        ));
        for (index, _) in compatible {
            covered[index] = true;
        }
    }

    for (index, signature) in signatures.iter().enumerate() {
        if !covered[index] {
            displays.insert(format_signature(call, signature));
        }
    }
    Ok(())
}

fn presented_args(kind: CallKind, signature: &SupportedSignature) -> &[SupportedArg] {
    let skip = usize::from(kind == CallKind::Method);
    signature.args().get(skip..).unwrap_or_default()
}

fn same_argument_metadata(
    kind: CallKind,
    left: &SupportedSignature,
    right: &SupportedSignature,
) -> bool {
    let left = presented_args(kind, left);
    let right = presented_args(kind, right);
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.argument_name() == right.argument_name()
                && left.has_default() == right.has_default()
        })
}

fn is_none(ty: &SupportedTy) -> bool {
    matches!(ty, SupportedTy::None { .. })
}

fn same_non_none_shape(
    kind: CallKind,
    left: &SupportedSignature,
    right: &SupportedSignature,
) -> bool {
    presented_args(kind, left)
        .iter()
        .zip(presented_args(kind, right))
        .all(|(left, right)| {
            (is_none(left.ty()) && is_none(right.ty())) || same_non_none_type(left.ty(), right.ty())
        })
}

fn same_non_none_type(left: &SupportedTy, right: &SupportedTy) -> bool {
    match (left, right) {
        (SupportedTy::Any { .. }, SupportedTy::Any { .. })
        | (SupportedTy::Bool { .. }, SupportedTy::Bool { .. })
        | (SupportedTy::Bytes { .. }, SupportedTy::Bytes { .. })
        | (SupportedTy::Date { .. }, SupportedTy::Date { .. })
        | (SupportedTy::DateTime { .. }, SupportedTy::DateTime { .. })
        | (SupportedTy::Float { .. }, SupportedTy::Float { .. })
        | (SupportedTy::HashlibHash { .. }, SupportedTy::HashlibHash { .. })
        | (SupportedTy::Int { .. }, SupportedTy::Int { .. })
        | (SupportedTy::Json { .. }, SupportedTy::Json { .. })
        | (SupportedTy::ReMatch { .. }, SupportedTy::ReMatch { .. })
        | (SupportedTy::RePattern { .. }, SupportedTy::RePattern { .. })
        | (SupportedTy::RequestsHttpResponse { .. }, SupportedTy::RequestsHttpResponse { .. })
        | (SupportedTy::SequenceMatcher { .. }, SupportedTy::SequenceMatcher { .. })
        | (SupportedTy::Str { .. }, SupportedTy::Str { .. })
        | (SupportedTy::Time { .. }, SupportedTy::Time { .. })
        | (SupportedTy::Timedelta { .. }, SupportedTy::Timedelta { .. })
        | (SupportedTy::TimeZone { .. }, SupportedTy::TimeZone { .. }) => true,
        (
            SupportedTy::Class {
                module: left_module,
                name: left_name,
                ..
            },
            SupportedTy::Class {
                module: right_module,
                name: right_name,
                ..
            },
        ) => left_module == right_module && left_name == right_name,
        (
            SupportedTy::Counter {
                items: left_items, ..
            },
            SupportedTy::Counter {
                items: right_items, ..
            },
        )
        | (
            SupportedTy::FrozenSet {
                items: left_items, ..
            },
            SupportedTy::FrozenSet {
                items: right_items, ..
            },
        )
        | (
            SupportedTy::Generator {
                items: left_items, ..
            },
            SupportedTy::Generator {
                items: right_items, ..
            },
        )
        | (
            SupportedTy::Iterable {
                items: left_items, ..
            },
            SupportedTy::Iterable {
                items: right_items, ..
            },
        )
        | (
            SupportedTy::List {
                items: left_items, ..
            },
            SupportedTy::List {
                items: right_items, ..
            },
        )
        | (
            SupportedTy::Set {
                items: left_items, ..
            },
            SupportedTy::Set {
                items: right_items, ..
            },
        ) => left_items == right_items,
        (
            SupportedTy::Dict {
                key_type: left_key,
                value_type: left_value,
                ..
            },
            SupportedTy::Dict {
                key_type: right_key,
                value_type: right_value,
                ..
            },
        ) => left_key == right_key && left_value == right_value,
        (
            SupportedTy::Module {
                name: left_name, ..
            },
            SupportedTy::Module {
                name: right_name, ..
            },
        )
        | (
            SupportedTy::Other {
                name: left_name, ..
            },
            SupportedTy::Other {
                name: right_name, ..
            },
        ) => left_name == right_name,
        (
            SupportedTy::SubClassOf {
                ty_name: left_name, ..
            },
            SupportedTy::SubClassOf {
                ty_name: right_name,
                ..
            },
        ) => left_name == right_name,
        (
            SupportedTy::Tuple {
                items: left_items,
                is_variable: left_is_variable,
                ..
            },
            SupportedTy::Tuple {
                items: right_items,
                is_variable: right_is_variable,
                ..
            },
        ) => left_items == right_items && left_is_variable == right_is_variable,
        _ => false,
    }
}

fn signature_matches_shape(
    kind: CallKind,
    signature: &SupportedSignature,
    shape: &SupportedSignature,
) -> bool {
    presented_args(kind, signature)
        .iter()
        .zip(presented_args(kind, shape))
        .all(|(arg, expected)| {
            is_none(arg.ty())
                || (!is_none(expected.ty()) && same_non_none_type(arg.ty(), expected.ty()))
        })
}

fn covers_full_cartesian_product(
    kind: CallKind,
    shape: &SupportedSignature,
    nullable: &[bool],
    signatures: &[(usize, &SupportedSignature)],
    budget: &mut PresentationBudget,
) -> Result<bool, BudgetExceeded> {
    let mut next_bit = 0;
    let axis_bits = nullable
        .iter()
        .map(|&nullable| {
            nullable.then(|| {
                let bit = next_bit;
                next_bit += 1;
                bit
            })
        })
        .collect::<Vec<_>>();
    let nullable_axis_count = u32::try_from(next_bit).map_err(|_| BudgetExceeded)?;
    let combination_count = 1usize
        .checked_shl(nullable_axis_count)
        .ok_or(BudgetExceeded)?;
    let shape_args = presented_args(kind, shape);
    budget.spend([combination_count, signatures.len(), shape_args.len().max(1)])?;

    Ok((0..combination_count).all(|combination| {
        signatures.iter().any(|(_, signature)| {
            presented_args(kind, signature)
                .iter()
                .enumerate()
                .all(|(index, arg)| {
                    let accepts_none =
                        axis_bits[index].is_some_and(|bit| combination & (1 << bit) != 0);
                    if accepts_none || is_none(shape_args[index].ty()) {
                        arg.ty().accepts_none()
                    } else {
                        same_non_none_type(arg.ty(), shape_args[index].ty())
                    }
                })
        })
    }))
}

fn format_signature(call: &SupportedCall, signature: &SupportedSignature) -> String {
    let args = presented_args(call.kind(), signature)
        .iter()
        .map(|arg| format_arg(format_supported_type(arg.ty()), arg))
        .collect::<Vec<_>>();
    format!("{}({})", call.name(), args.join(", "))
}

fn format_collapsed_signature(
    call: &SupportedCall,
    metadata: &SupportedSignature,
    shape: &SupportedSignature,
    nullable: &[bool],
) -> String {
    let args = presented_args(call.kind(), metadata)
        .iter()
        .zip(presented_args(call.kind(), shape))
        .zip(nullable)
        .map(|((arg, shape), &nullable)| {
            let ty = format_supported_type_with_nullable(shape.ty(), nullable);
            format_arg(ty, arg)
        })
        .collect::<Vec<_>>();
    format!("{}({})", call.name(), args.join(", "))
}

fn format_arg(mut ty: String, arg: &SupportedArg) -> String {
    if let Some(name) = arg.argument_name() {
        ty = format!("{name}: {ty}");
    }
    if arg.has_default() {
        ty.push_str(" = ...");
    }
    ty
}

fn format_supported_type(ty: &SupportedTy) -> String {
    let nullable = ty.accepts_none() && !matches!(ty, SupportedTy::None { .. });
    format_supported_type_with_nullable(ty, nullable)
}

fn format_supported_type_with_nullable(ty: &SupportedTy, nullable: bool) -> String {
    let display = match ty {
        SupportedTy::Any { .. } => "Any".to_owned(),
        SupportedTy::Bool { .. } => "bool".to_owned(),
        SupportedTy::Bytes { .. } => "bytes".to_owned(),
        SupportedTy::Class { module, name, .. } => format!("class[{module}.{name}]"),
        SupportedTy::Counter { items, .. } => {
            format!("Counter[{}]", format_supported_type(items))
        }
        SupportedTy::Date { .. } => "date".to_owned(),
        SupportedTy::DateTime { .. } => "datetime".to_owned(),
        SupportedTy::Dict {
            key_type,
            value_type,
            ..
        } => format!(
            "dict[{}, {}]",
            format_supported_type(key_type),
            format_supported_type(value_type)
        ),
        SupportedTy::Float { .. } => "float".to_owned(),
        SupportedTy::FrozenSet { items, .. } => {
            format!("frozenset[{}]", format_supported_type(items))
        }
        SupportedTy::Generator { items, .. } => {
            format!("Generator[{}]", format_supported_type(items))
        }
        SupportedTy::HashlibHash { .. } => "hashlib.Hash".to_owned(),
        SupportedTy::Int { .. } => "int".to_owned(),
        SupportedTy::Iterable { items, .. } => {
            format!("Iterable[{}]", format_supported_type(items))
        }
        SupportedTy::Json { .. } => "json".to_owned(),
        SupportedTy::List { items, .. } => {
            format!("list[{}]", format_supported_type(items))
        }
        SupportedTy::Module { name, .. } => format!("module[{name}]"),
        SupportedTy::None { .. } => "None".to_owned(),
        SupportedTy::ReMatch { .. } => "Match".to_owned(),
        SupportedTy::RePattern { .. } => "Pattern".to_owned(),
        SupportedTy::RequestsHttpResponse { .. } => "RequestsHttpResponse".to_owned(),
        SupportedTy::Set { items, .. } => {
            format!("set[{}]", format_supported_type(items))
        }
        SupportedTy::SequenceMatcher { .. } => "SequenceMatcher".to_owned(),
        SupportedTy::Str { .. } => "str".to_owned(),
        SupportedTy::SubClassOf { ty_name, .. } => format!("subclass[{ty_name}]"),
        SupportedTy::Time { .. } => "time".to_owned(),
        SupportedTy::Timedelta { .. } => "timedelta".to_owned(),
        SupportedTy::TimeZone { .. } => "timezone".to_owned(),
        SupportedTy::Tuple {
            items, is_variable, ..
        } => {
            if *is_variable {
                format!(
                    "tuple[{}, ...]",
                    items
                        .first()
                        .map_or_else(|| "Any".to_owned(), format_supported_type)
                )
            } else {
                format!(
                    "tuple[{}]",
                    items
                        .iter()
                        .map(format_supported_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        SupportedTy::Other { name, .. } => name.clone(),
    };

    if nullable && !matches!(ty, SupportedTy::None { .. }) {
        format!("{display} | None")
    } else {
        display
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn call(kind: CallKind, name: &str) -> SupportedCall {
        SupportedCall {
            kind,
            name: name.to_owned(),
        }
    }

    fn signature(args: Vec<SupportedArg>) -> SupportedSignature {
        SupportedSignature {
            args: args.into_boxed_slice(),
        }
    }

    fn arg(ty: SupportedTy) -> SupportedArg {
        SupportedArg {
            ty,
            argument_name: None,
            has_default: false,
        }
    }

    fn named_arg(ty: SupportedTy, name: &str, has_default: bool) -> SupportedArg {
        SupportedArg {
            ty,
            argument_name: Some(name.to_owned()),
            has_default,
        }
    }

    fn supported(
        entries: impl IntoIterator<Item = (SupportedCall, Vec<SupportedSignature>)>,
    ) -> SupportedFuncs {
        SupportedFuncs::from_impls(entries.into_iter().collect::<BTreeMap<_, _>>())
    }

    fn int() -> SupportedTy {
        SupportedTy::Int { nullable: false }
    }

    fn none() -> SupportedTy {
        SupportedTy::None { nullable: true }
    }

    #[test]
    fn builtin_keeps_first_argument_and_method_omits_receiver() {
        let len = call(CallKind::Builtin, "len");
        let method = call(CallKind::Method, "method");
        let supported = supported([
            (len.clone(), vec![signature(vec![arg(int())])]),
            (
                method.clone(),
                vec![signature(vec![
                    arg(SupportedTy::Other {
                        nullable: false,
                        name: "Receiver".to_owned(),
                    }),
                    arg(int()),
                ])],
            ),
        ]);

        assert_eq!(
            present_supported_signatures(&supported, &[method, len]),
            ["len(int)", "method(int)"]
        );
    }

    #[test]
    fn method_receiver_is_projected_before_grouping_and_nullable_collapse() {
        let method = call(CallKind::Method, "method");
        let supported = supported([(
            method.clone(),
            vec![
                signature(vec![
                    named_arg(
                        SupportedTy::Other {
                            nullable: false,
                            name: "FirstReceiver".to_owned(),
                        },
                        "first_self",
                        false,
                    ),
                    named_arg(int(), "value", false),
                ]),
                signature(vec![
                    named_arg(
                        SupportedTy::Other {
                            nullable: false,
                            name: "SecondReceiver".to_owned(),
                        },
                        "second_self",
                        true,
                    ),
                    named_arg(none(), "value", false),
                ]),
            ],
        )]);

        assert_eq!(
            present_supported_signatures(&supported, &[method]),
            ["method(value: int | None)"]
        );
    }

    #[test]
    fn preserves_argument_names_and_defaults() {
        let pow = call(CallKind::Builtin, "pow");
        let supported = supported([(
            pow.clone(),
            vec![signature(vec![
                named_arg(int(), "base", false),
                named_arg(int(), "exponent", true),
            ])],
        )]);

        assert_eq!(
            present_supported_signatures(&supported, &[pow]),
            ["pow(base: int, exponent: int = ...)"]
        );
    }

    #[test]
    fn formats_every_supported_type_category_and_recursive_type() {
        let types = [
            SupportedTy::Any { nullable: false },
            SupportedTy::Bool { nullable: false },
            SupportedTy::Bytes { nullable: false },
            SupportedTy::Class {
                nullable: true,
                module: "models".to_owned(),
                name: "User".to_owned(),
            },
            SupportedTy::Counter {
                nullable: false,
                items: Box::new(int()),
            },
            SupportedTy::Date { nullable: false },
            SupportedTy::DateTime { nullable: false },
            SupportedTy::Dict {
                nullable: false,
                key_type: Box::new(SupportedTy::Str { nullable: false }),
                value_type: Box::new(SupportedTy::List {
                    nullable: false,
                    items: Box::new(SupportedTy::Int { nullable: true }),
                }),
            },
            SupportedTy::Float { nullable: false },
            SupportedTy::FrozenSet {
                nullable: false,
                items: Box::new(SupportedTy::Str { nullable: false }),
            },
            SupportedTy::Generator {
                nullable: false,
                items: Box::new(SupportedTy::Bytes { nullable: false }),
            },
            SupportedTy::HashlibHash { nullable: false },
            int(),
            SupportedTy::Iterable {
                nullable: false,
                items: Box::new(SupportedTy::Any { nullable: false }),
            },
            SupportedTy::Json { nullable: false },
            SupportedTy::List {
                nullable: false,
                items: Box::new(int()),
            },
            SupportedTy::Module {
                nullable: false,
                name: "math".to_owned(),
            },
            none(),
            SupportedTy::ReMatch { nullable: false },
            SupportedTy::RePattern { nullable: false },
            SupportedTy::RequestsHttpResponse { nullable: false },
            SupportedTy::Set {
                nullable: false,
                items: Box::new(int()),
            },
            SupportedTy::SequenceMatcher { nullable: false },
            SupportedTy::Str { nullable: false },
            SupportedTy::SubClassOf {
                ty_name: "Proto".to_owned(),
                match_nullable: true,
            },
            SupportedTy::Time { nullable: false },
            SupportedTy::Timedelta { nullable: false },
            SupportedTy::TimeZone { nullable: false },
            SupportedTy::Tuple {
                nullable: false,
                items: vec![int(), SupportedTy::Str { nullable: false }],
                is_variable: false,
            },
            SupportedTy::Tuple {
                nullable: false,
                items: vec![SupportedTy::Float { nullable: false }],
                is_variable: true,
            },
            SupportedTy::Other {
                nullable: true,
                name: "Custom".to_owned(),
            },
        ];

        assert_eq!(
            types.iter().map(format_supported_type).collect::<Vec<_>>(),
            [
                "Any",
                "bool",
                "bytes",
                "class[models.User] | None",
                "Counter[int]",
                "date",
                "datetime",
                "dict[str, list[int | None]]",
                "float",
                "frozenset[str]",
                "Generator[bytes]",
                "hashlib.Hash",
                "int",
                "Iterable[Any]",
                "json",
                "list[int]",
                "module[math]",
                "None",
                "Match",
                "Pattern",
                "RequestsHttpResponse",
                "set[int]",
                "SequenceMatcher",
                "str",
                "subclass[Proto] | None",
                "time",
                "timedelta",
                "timezone",
                "tuple[int, str]",
                "tuple[float, ...]",
                "Custom | None",
            ]
        );
    }

    #[test]
    fn deduplicates_and_orders_independently_of_call_and_signature_order() {
        let alpha = call(CallKind::Builtin, "alpha");
        let beta = call(CallKind::Builtin, "beta");
        let forward_supported = supported([
            (
                alpha.clone(),
                vec![
                    signature(vec![arg(int())]),
                    signature(vec![arg(SupportedTy::Str { nullable: false })]),
                ],
            ),
            (
                beta.clone(),
                vec![signature(vec![arg(SupportedTy::Bool { nullable: false })])],
            ),
        ]);
        let reverse_supported = supported([
            (
                alpha.clone(),
                vec![
                    signature(vec![arg(SupportedTy::Str { nullable: false })]),
                    signature(vec![arg(int())]),
                ],
            ),
            (
                beta.clone(),
                vec![signature(vec![arg(SupportedTy::Bool { nullable: false })])],
            ),
        ]);

        let forward =
            present_supported_signatures(&forward_supported, &[alpha.clone(), beta.clone()]);
        let reverse =
            present_supported_signatures(&reverse_supported, &[beta, alpha.clone(), alpha]);
        assert_eq!(forward, ["alpha(int)", "alpha(str)", "... [1 more]"]);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn applies_one_global_cap_and_ignores_missing_registry_calls() {
        let alpha = call(CallKind::Builtin, "alpha");
        let beta = call(CallKind::Builtin, "beta");
        let gamma = call(CallKind::Builtin, "gamma");
        let delta = call(CallKind::Builtin, "delta");
        let missing = call(CallKind::Builtin, "missing");
        let supported = supported([
            (alpha.clone(), vec![signature(vec![arg(int())])]),
            (beta.clone(), vec![signature(vec![arg(int())])]),
            (gamma.clone(), vec![signature(vec![arg(int())])]),
            (delta.clone(), vec![signature(vec![arg(int())])]),
        ]);

        assert_eq!(
            present_supported_signatures(&supported, &[missing, gamma, beta, delta, alpha]),
            ["alpha(int)", "beta(int)", "... [2 more]"]
        );
    }

    #[test]
    fn collapses_only_a_complete_top_level_nullable_cartesian_product() {
        let function = call(CallKind::Builtin, "f");
        let supported = supported([(
            function.clone(),
            vec![
                signature(vec![
                    named_arg(int(), "left", false),
                    named_arg(int(), "right", true),
                ]),
                signature(vec![
                    named_arg(int(), "left", false),
                    named_arg(none(), "right", true),
                ]),
                signature(vec![
                    named_arg(none(), "left", false),
                    named_arg(int(), "right", true),
                ]),
                signature(vec![
                    named_arg(none(), "left", false),
                    named_arg(none(), "right", true),
                ]),
            ],
        )]);

        assert_eq!(
            present_supported_signatures(&supported, &[function]),
            ["f(left: int | None, right: int | None = ...)"]
        );
    }

    #[test]
    fn diagonal_and_incomplete_nullable_products_stay_separate() -> Result<(), BudgetExceeded> {
        let diagonal = call(CallKind::Builtin, "diagonal");
        let incomplete = call(CallKind::Builtin, "incomplete");
        let diagonal_signatures = vec![
            signature(vec![arg(int()), arg(int())]),
            signature(vec![arg(none()), arg(none())]),
        ];
        let incomplete_signatures = vec![
            signature(vec![arg(int()), arg(int())]),
            signature(vec![arg(int()), arg(none())]),
            signature(vec![arg(none()), arg(none())]),
        ];

        let mut budget = PresentationBudget::new();
        assert_eq!(
            displays_for_call(&diagonal, &diagonal_signatures, &mut budget)?
                .into_iter()
                .collect::<Vec<_>>(),
            ["diagonal(None, None)", "diagonal(int, int)"]
        );
        assert_eq!(
            displays_for_call(&incomplete, &incomplete_signatures, &mut budget)?
                .into_iter()
                .collect::<Vec<_>>(),
            [
                "incomplete(None, None)",
                "incomplete(int, None)",
                "incomplete(int, int)",
            ]
        );
        Ok(())
    }

    #[test]
    fn nullable_diagonal_stays_separate() {
        let function = call(CallKind::Builtin, "f");
        let supported = supported([(
            function.clone(),
            vec![
                signature(vec![arg(SupportedTy::Int { nullable: true }), arg(int())]),
                signature(vec![arg(int()), arg(SupportedTy::Int { nullable: true })]),
            ],
        )]);

        assert_eq!(
            present_supported_signatures(&supported, &[function]),
            ["f(int | None, int)", "f(int, int | None)"]
        );
    }

    #[test]
    fn budget_exhaustion_falls_back_to_raw_signatures() {
        let function = call(CallKind::Builtin, "f");
        let nullable_args = (0..17)
            .map(|_| arg(SupportedTy::Int { nullable: true }))
            .collect();
        let non_nullable_args = (0..17).map(|_| arg(int())).collect();
        let supported = supported([(
            function.clone(),
            vec![signature(nullable_args), signature(non_nullable_args)],
        )]);

        let displays = present_supported_signatures(&supported, &[function]);
        assert_eq!(displays.len(), 2);
        assert!(displays[0].contains(" | None"));
        assert!(!displays[1].contains(" | None"));
    }

    #[test]
    fn does_not_collapse_different_metadata_non_null_or_nested_structures()
    -> Result<(), BudgetExceeded> {
        let function = call(CallKind::Builtin, "f");
        let signatures = vec![
            signature(vec![named_arg(int(), "value", false)]),
            signature(vec![named_arg(none(), "other", false)]),
            signature(vec![named_arg(none(), "value", true)]),
            signature(vec![named_arg(
                SupportedTy::Str { nullable: false },
                "value",
                false,
            )]),
            signature(vec![arg(SupportedTy::List {
                nullable: false,
                items: Box::new(int()),
            })]),
            signature(vec![arg(SupportedTy::List {
                nullable: false,
                items: Box::new(SupportedTy::Int { nullable: true }),
            })]),
        ];

        let mut budget = PresentationBudget::new();
        assert_eq!(
            displays_for_call(&function, &signatures, &mut budget)?
                .into_iter()
                .collect::<Vec<_>>(),
            [
                "f(list[int | None])",
                "f(list[int])",
                "f(other: None)",
                "f(value: None = ...)",
                "f(value: int)",
                "f(value: str)",
            ]
        );
        Ok(())
    }
}
