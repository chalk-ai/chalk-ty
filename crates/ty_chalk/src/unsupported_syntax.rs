use ruff_python_ast as ast;
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::registry_operations::{binary_method, unary_method};

/// Syntax forms that the accelerator rejects before consulting the function registry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum UnsupportedSyntaxKind {
    DictionaryComprehension,
    DictionaryUnpacking,
    MultipleGeneratorListComprehension,
    ComplexNumberLiteral,
    UnsupportedExpression(&'static str),
    UnsupportedStatement(&'static str),
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
        ast::Expr::Dict(dictionary) if dictionary.items.iter().any(|item| item.key.is_none()) => {
            UnsupportedSyntaxKind::DictionaryUnpacking
        }
        ast::Expr::ListComp(comprehension) if comprehension.generators.len() > 1 => {
            UnsupportedSyntaxKind::MultipleGeneratorListComprehension
        }
        ast::Expr::NumberLiteral(number) if matches!(number.value, ast::Number::Complex { .. }) => {
            UnsupportedSyntaxKind::ComplexNumberLiteral
        }
        ast::Expr::Named(_) => {
            UnsupportedSyntaxKind::UnsupportedExpression("Assignment expressions")
        }
        ast::Expr::Await(_) => UnsupportedSyntaxKind::UnsupportedExpression("Await expressions"),
        ast::Expr::Yield(_) => UnsupportedSyntaxKind::UnsupportedExpression("Yield expressions"),
        ast::Expr::YieldFrom(_) => {
            UnsupportedSyntaxKind::UnsupportedExpression("Yield-from expressions")
        }
        ast::Expr::EllipsisLiteral(_) => {
            UnsupportedSyntaxKind::UnsupportedExpression("Ellipsis literals")
        }
        ast::Expr::Starred(_) => {
            UnsupportedSyntaxKind::UnsupportedExpression("Starred expressions")
        }
        ast::Expr::TString(_) => UnsupportedSyntaxKind::UnsupportedExpression("Template strings"),
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
        ast::Stmt::FunctionDef(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Nested function definitions"),
        ),
        ast::Stmt::ClassDef(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Class definitions"),
        ),
        ast::Stmt::Delete(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Delete statements"),
        ),
        ast::Stmt::With(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("With statements"),
        ),
        ast::Stmt::Match(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Match statements"),
        ),
        ast::Stmt::For(for_statement) if for_statement.is_async => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Async for statements"),
        ),
        ast::Stmt::Import(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Imports inside functions"),
        ),
        ast::Stmt::ImportFrom(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Imports inside functions"),
        ),
        ast::Stmt::Global(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Global declarations"),
        ),
        ast::Stmt::Nonlocal(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Nonlocal declarations"),
        ),
        ast::Stmt::TypeAlias(_) => (
            statement.range(),
            UnsupportedSyntaxKind::UnsupportedStatement("Type aliases inside functions"),
        ),
        _ => return None,
    };
    Some(UnsupportedSyntax { range, kind })
}
