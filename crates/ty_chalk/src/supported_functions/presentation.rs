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
