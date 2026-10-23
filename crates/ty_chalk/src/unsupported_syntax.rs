use ruff_python_ast as ast;
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::registry_operations::{binary_method, unary_method};

/// Syntax forms that the accelerator rejects before consulting the function registry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum UnsupportedSyntaxKind {
    DictionaryComprehension,
    MultipleGeneratorListComprehension,
    Operator(&'static str),
    WhileStatement,
    ComplexAugmentedAssignmentTarget,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UnsupportedSyntax {
    pub(crate) range: TextRange,
    pub(crate) kind: UnsupportedSyntaxKind,
}

/// Classifies an expression that the accelerator rejects structurally.
pub(crate) fn unsupported_expression(expression: &ast::Expr) -> Option<UnsupportedSyntax> {
    let kind = match expression {
        ast::Expr::BinOp(binary) if binary_method(binary.op).is_none() => {
            UnsupportedSyntaxKind::Operator(binary.op.as_str())
        }
        ast::Expr::UnaryOp(unary) if unary_method(unary.op).is_none() => {
            UnsupportedSyntaxKind::Operator(unary.op.as_str())
        }
        ast::Expr::DictComp(_) => UnsupportedSyntaxKind::DictionaryComprehension,
        ast::Expr::ListComp(comprehension) if comprehension.generators.len() > 1 => {
            UnsupportedSyntaxKind::MultipleGeneratorListComprehension
        }
        _ => return None,
    };
    Some(UnsupportedSyntax {
        range: expression.range(),
        kind,
    })
}

/// Classifies a statement that the accelerator rejects structurally.
pub(crate) fn unsupported_statement(statement: &ast::Stmt) -> Option<UnsupportedSyntax> {
    let (range, kind) = match statement {
        ast::Stmt::While(while_statement) => (
            TextRange::at(while_statement.start(), TextSize::new(5)),
            UnsupportedSyntaxKind::WhileStatement,
        ),
        ast::Stmt::AugAssign(assignment) if !assignment.target.is_name_expr() => (
            assignment.target.range(),
            UnsupportedSyntaxKind::ComplexAugmentedAssignmentTarget,
        ),
        ast::Stmt::AugAssign(assignment) if binary_method(assignment.op).is_none() => (
            assignment.range,
            UnsupportedSyntaxKind::Operator(assignment.op.as_str()),
        ),
        _ => return None,
    };
    Some(UnsupportedSyntax { range, kind })
}
