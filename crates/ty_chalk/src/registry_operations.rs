use ruff_python_ast as ast;
use ruff_text_size::{Ranged, TextRange};
use ty_python_semantic::Db;
use ty_python_semantic::types::Type;

/// One Python syntax operation that may be lowered to the static accelerator's function registry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RegistryOperation<'a> {
    pub(crate) range: TextRange,
    pub(crate) method: &'static str,
    pub(crate) receiver: &'a ast::Expr,
    pub(crate) argument: Option<&'a ast::Expr>,
}

impl<'a> RegistryOperation<'a> {
    fn unary(range: TextRange, method: &'static str, receiver: &'a ast::Expr) -> Self {
        Self {
            range,
            method,
            receiver,
            argument: None,
        }
    }

    fn binary(
        range: TextRange,
        method: &'static str,
        receiver: &'a ast::Expr,
        argument: &'a ast::Expr,
    ) -> Self {
        Self {
            range,
            method,
            receiver,
            argument: Some(argument),
        }
    }

    fn truthiness(expression: &'a ast::Expr) -> Self {
        Self::unary(expression.range(), "__bool__", expression)
    }

    /// Returns whether the accelerator handles this operation without consulting the registry.
    pub(crate) fn bypasses_registry<'db>(
        self,
        db: &'db dyn Db,
        receiver: Type<'db>,
        argument: Option<Type<'db>>,
    ) -> bool {
        matches!(self.method, "__eq__" | "__neq__")
            && (receiver.is_none(db) || argument.is_some_and(|ty| ty.is_none(db)))
    }
}

/// Identifies expression operations that may dispatch through the accelerator registry.
pub(crate) fn expression_registry_operations(expression: &ast::Expr) -> Vec<RegistryOperation<'_>> {
    match expression {
        ast::Expr::BinOp(binary) => binary_method(binary.op)
            .map(|method| {
                RegistryOperation::binary(binary.range, method, &binary.left, &binary.right)
            })
            .into_iter()
            .collect(),
        ast::Expr::UnaryOp(unary) => unary_method(unary.op)
            .map(|method| RegistryOperation::unary(unary.range, method, &unary.operand))
            .into_iter()
            .collect(),
        ast::Expr::Subscript(subscript) if subscript.ctx == ast::ExprContext::Load => {
            vec![RegistryOperation::binary(
                subscript.range,
                "__getitem__",
                &subscript.value,
                &subscript.slice,
            )]
        }
        ast::Expr::Compare(comparison) => comparison_operations(comparison),
        ast::Expr::BoolOp(boolean) => boolean
            .values
            .iter()
            .take(boolean.values.len().saturating_sub(1))
            .map(RegistryOperation::truthiness)
            .collect(),
        ast::Expr::If(conditional) => {
            vec![RegistryOperation::truthiness(&conditional.test)]
        }
        _ => Vec::new(),
    }
}

/// Identifies statement operations that may dispatch through the accelerator registry.
pub(crate) fn statement_registry_operations(statement: &ast::Stmt) -> Vec<RegistryOperation<'_>> {
    match statement {
        ast::Stmt::AugAssign(assignment) if assignment.target.is_name_expr() => {
            binary_method(assignment.op)
                .map(|method| {
                    RegistryOperation::binary(
                        assignment.range,
                        method,
                        &assignment.target,
                        &assignment.value,
                    )
                })
                .into_iter()
                .collect()
        }
        ast::Stmt::If(conditional) => {
            std::iter::once(RegistryOperation::truthiness(&conditional.test))
                .chain(
                    conditional
                        .elif_else_clauses
                        .iter()
                        .filter_map(|clause| clause.test.as_ref())
                        .map(RegistryOperation::truthiness),
                )
                .collect()
        }
        ast::Stmt::Assert(assertion) => {
            vec![RegistryOperation::truthiness(&assertion.test)]
        }
        _ => Vec::new(),
    }
}

/// Identifies the potential registry calls for comprehension-filter truthiness checks.
pub(crate) fn comprehension_registry_operations(
    comprehension: &ast::Comprehension,
) -> impl Iterator<Item = RegistryOperation<'_>> {
    comprehension.ifs.iter().map(RegistryOperation::truthiness)
}

// These are the operators dispatched by the accelerator. The names use the standard Python
// protocol vocabulary except for the accelerator's legacy spelling of true division.
pub(super) const fn binary_method(operator: ast::Operator) -> Option<&'static str> {
    match operator {
        ast::Operator::Add
        | ast::Operator::Sub
        | ast::Operator::Mult
        | ast::Operator::Mod
        | ast::Operator::Pow
        | ast::Operator::FloorDiv
        | ast::Operator::BitOr
        | ast::Operator::BitAnd => Some(operator.dunder()),
        ast::Operator::Div => Some("__div__"),
        ast::Operator::MatMult
        | ast::Operator::LShift
        | ast::Operator::RShift
        | ast::Operator::BitXor => None,
    }
}

pub(super) const fn unary_method(operator: ast::UnaryOp) -> Option<&'static str> {
    match operator {
        ast::UnaryOp::UAdd => Some("__pos__"),
        ast::UnaryOp::USub => Some("__neg__"),
        ast::UnaryOp::Not => Some("__bool__"),
        ast::UnaryOp::Invert => None,
    }
}

fn comparison_operations(comparison: &ast::ExprCompare) -> Vec<RegistryOperation<'_>> {
    let operands = std::iter::once(comparison.left.as_ref()).chain(&comparison.comparators);
    let mut pairs = operands.clone().zip(operands.skip(1));
    comparison
        .ops
        .iter()
        .filter_map(|operator| {
            let (left, right) = pairs.next()?;
            let (method, receiver, argument) = match operator {
                ast::CmpOp::Eq => ("__eq__", left, right),
                ast::CmpOp::NotEq => ("__neq__", left, right),
                ast::CmpOp::Lt => ("__lt__", left, right),
                ast::CmpOp::LtE => ("__le__", left, right),
                ast::CmpOp::Gt => ("__gt__", left, right),
                ast::CmpOp::GtE => ("__ge__", left, right),
                ast::CmpOp::In | ast::CmpOp::NotIn => ("__contains__", right, left),
                ast::CmpOp::Is | ast::CmpOp::IsNot => return None,
            };
            Some(RegistryOperation::binary(
                comparison.range,
                method,
                receiver,
                argument,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{binary_method, unary_method};
    use ruff_python_ast as ast;

    #[test]
    fn binary_names_match_accelerator_registry_vocabulary() {
        assert_eq!(binary_method(ast::Operator::Add), Some("__add__"));
        assert_eq!(binary_method(ast::Operator::Sub), Some("__sub__"));
        assert_eq!(binary_method(ast::Operator::Mult), Some("__mul__"));
        assert_eq!(binary_method(ast::Operator::MatMult), None);
        assert_eq!(binary_method(ast::Operator::Div), Some("__div__"));
        assert_eq!(binary_method(ast::Operator::Mod), Some("__mod__"));
        assert_eq!(binary_method(ast::Operator::Pow), Some("__pow__"));
        assert_eq!(binary_method(ast::Operator::LShift), None);
        assert_eq!(binary_method(ast::Operator::RShift), None);
        assert_eq!(binary_method(ast::Operator::BitOr), Some("__or__"));
        assert_eq!(binary_method(ast::Operator::BitXor), None);
        assert_eq!(binary_method(ast::Operator::BitAnd), Some("__and__"));
        assert_eq!(binary_method(ast::Operator::FloorDiv), Some("__floordiv__"));
    }

    #[test]
    fn unary_names_match_accelerator_registry_vocabulary() {
        assert_eq!(unary_method(ast::UnaryOp::UAdd), Some("__pos__"));
        assert_eq!(unary_method(ast::UnaryOp::USub), Some("__neg__"));
        assert_eq!(unary_method(ast::UnaryOp::Not), Some("__bool__"));
        assert_eq!(unary_method(ast::UnaryOp::Invert), None);
    }
}
