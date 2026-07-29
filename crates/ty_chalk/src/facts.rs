use std::panic::AssertUnwindSafe;

use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_expr, walk_stmt};
use ruff_python_ast::{self as ast};
use ruff_text_size::{Ranged, TextRange};
use ty_python_semantic::chalk::{CallTarget, Definition, ModuleOrigin};
use ty_python_semantic::{Db, HasDefinition, HasType, SemanticModel};

use crate::CallNoMatchReason;
use crate::call_matcher::{
    CallMatch, CallMatchTarget, ObservedArgument, ObservedCall, ObservedCallTarget,
    match_call_target,
};
use crate::suppression::{SuppressionCode, SuppressionProblem, Suppressions, extract_suppressions};

/// A named function recognized as a baked Chalk resolver root.
#[derive(Clone, Copy, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct ResolverRootFact<'db> {
    pub(crate) definition: Definition<'db>,
    pub(crate) range: TextRange,
    /// The exact function-level directive suppressing this root, if any.
    pub(crate) unsupported_function_suppression: Option<TextRange>,
}

/// A call expression physically contained in a named function body.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct CallFact<'db> {
    pub(crate) caller: Definition<'db>,
    pub(crate) range: TextRange,
    pub(crate) targets: Box<[CallTarget<'db>]>,
    pub(crate) no_matches: Box<[(CallMatchTarget<'db>, CallNoMatchReason)]>,
    /// The exact statement-level directive suppressing this call, if any.
    pub(crate) unsupported_function_statement_suppression: Option<TextRange>,
    /// The exact function-level directive suppressing this call's caller, if any.
    pub(crate) unsupported_function_caller_suppression: Option<TextRange>,
}

/// Compact Chalk-specific semantic facts extracted from one Python file.
#[derive(Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub(crate) struct FileFacts<'db> {
    resolver_roots: Box<[ResolverRootFact<'db>]>,
    calls: Box<[CallFact<'db>]>,
    suppression_problems: Box<[SuppressionProblem]>,
}

impl<'db> FileFacts<'db> {
    pub(crate) fn resolver_roots(&self) -> &[ResolverRootFact<'db>] {
        &self.resolver_roots
    }

    pub(crate) fn calls(&self) -> &[CallFact<'db>] {
        &self.calls
    }

    pub(crate) fn suppression_problems(&self) -> &[SuppressionProblem] {
        &self.suppression_problems
    }
}

/// Extracts source-ordered Chalk facts without retaining AST nodes or general native types.
fn extract_file_facts(db: &dyn Db, file: File) -> FileFacts<'_> {
    let parsed = parsed_module(db, file).load(db);
    let model = SemanticModel::new(db, file);
    let suppressions = extract_suppressions(db, file);
    let mut visitor = FactVisitor {
        model: &model,
        suppressions: &suppressions,
        caller: None,
        statement: None,
        resolver_roots: Vec::new(),
        calls: Vec::new(),
    };
    visitor.visit_body(&parsed.syntax().body);

    FileFacts {
        resolver_roots: visitor.resolver_roots.into_boxed_slice(),
        calls: visitor.calls.into_boxed_slice(),
        suppression_problems: suppressions.problems().into(),
    }
}

/// Returns cached Chalk facts after the file completes ordinary semantic analysis.
///
/// Recoverable parse and type errors do not discard facts. A fatal analysis abort, such as a
/// source read failure, returns `None`.
#[salsa::tracked(returns(as_ref), heap_size=ruff_memory_usage::heap_size)]
pub(crate) fn file_facts(db: &dyn Db, file: File) -> Option<FileFacts<'_>> {
    let unwind_safe_db = AssertUnwindSafe(db);
    match ruff_db::panic::catch_unwind(|| {
        ty_python_semantic::check_file(*unwind_safe_db, file).ok()?;
        Some(extract_file_facts(*unwind_safe_db, file))
    }) {
        Ok(facts) => facts,
        Err(error) => {
            match error.payload.downcast_ref::<salsa::Cancelled>() {
                None => {}
                Some(salsa::Cancelled::PropagatedPanic) => {
                    db.unwind_if_revision_cancelled();
                }
                Some(_) => error.resume_unwind(),
            }

            // Panicked queries do not preserve their dependencies for Salsa.
            db.report_untracked_read();
            None
        }
    }
}

struct FactVisitor<'a, 'db> {
    model: &'a SemanticModel<'db>,
    suppressions: &'a Suppressions<'db>,
    caller: Option<Definition<'db>>,
    statement: Option<TextRange>,
    resolver_roots: Vec<ResolverRootFact<'db>>,
    calls: Vec<CallFact<'db>>,
}

impl<'a, 'db> FactVisitor<'a, 'db> {
    fn visit_function(&mut self, function: &'a ast::StmtFunctionDef) {
        let definition = function.definition(self.model);

        if function
            .decorator_list
            .iter()
            .any(|decorator| self.is_resolver_decorator(decorator))
        {
            self.resolver_roots.push(ResolverRootFact {
                definition,
                range: function.range,
                unsupported_function_suppression: self.function_suppression(definition),
            });
        }

        let enclosing_caller = self.caller.replace(definition);
        self.visit_body(&function.body);
        self.caller = enclosing_caller;
    }

    fn is_resolver_decorator(&self, decorator: &ast::Decorator) -> bool {
        let provenance = self.model.chalk_decorator_provenance(decorator);

        if !provenance.definitions.is_empty() {
            !provenance.has_unresolved
                && provenance.definitions.iter().all(|definition| {
                    self.model
                        .chalk_decorator_definition_origin(*definition)
                        .is_some_and(|origin| {
                            is_baked_resolver_origin(
                                origin.ownership_origin(),
                                origin.module_name(),
                                origin.symbol_name(),
                            )
                        })
                })
        } else {
            provenance.module_fallback_is_complete
                && !provenance.modules.is_empty()
                && provenance.modules.iter().all(|origin| {
                    is_baked_resolver_origin(
                        origin.ownership_origin(),
                        origin.module_name(),
                        origin.symbol_name(),
                    )
                })
        }
    }

    fn function_suppression(&self, definition: Definition<'db>) -> Option<TextRange> {
        self.suppressions
            .function()
            .iter()
            .find(|suppression| {
                suppression.definition == definition
                    && suppression.code == SuppressionCode::UnsupportedFunction
            })
            .map(|suppression| suppression.directive)
    }

    fn statement_suppression(&self, statement: TextRange) -> Option<TextRange> {
        self.suppressions
            .statement()
            .iter()
            .find(|suppression| {
                suppression.statement == statement
                    && suppression.code == SuppressionCode::UnsupportedFunction
            })
            .map(|suppression| suppression.directive)
    }

    fn visit_call(&mut self, call: &ast::ExprCall) {
        let (Some(caller), Some(statement)) = (self.caller, self.statement) else {
            return;
        };
        let targets = self.model.chalk_call_targets(call);
        let module_provenance = self.model.chalk_call_module_provenance(call);
        let has_argument_unpacking = call.arguments.args.iter().any(ast::Expr::is_starred_expr)
            || call
                .arguments
                .keywords
                .iter()
                .any(|keyword| keyword.arg.is_none());
        let arguments = if has_argument_unpacking {
            None
        } else {
            call.arguments
                .iter_source_order()
                .map(|argument| match argument {
                    ast::ArgOrKeyword::Arg(argument) => argument
                        .inferred_type(self.model)
                        .map(ObservedArgument::Positional),
                    ast::ArgOrKeyword::Keyword(keyword) => Some(ObservedArgument::Keyword {
                        name: keyword.arg.as_ref()?.id.as_str(),
                        ty: keyword.value.inferred_type(self.model)?,
                    }),
                })
                .collect::<Option<Vec<_>>>()
        };
        let observed_arguments = arguments.as_deref().unwrap_or_default();
        let receiver = call
            .func
            .as_attribute_expr()
            .and_then(|attribute| attribute.value.inferred_type(self.model));
        let defer_matching = arguments.is_none();
        let observed_receiver = if defer_matching { None } else { receiver };
        let mut no_matches = Vec::new();
        let mut record_match = |target| {
            let CallMatch::NoMatch { target, reason } = match_call_target(
                self.model.db(),
                ObservedCall {
                    target: if defer_matching {
                        ObservedCallTarget::Deferred
                    } else {
                        target
                    },
                    arguments: observed_arguments,
                    receiver: observed_receiver,
                },
            ) else {
                return;
            };
            let no_match = (target, reason);
            if !no_matches.contains(&no_match) {
                no_matches.push(no_match);
            }
        };
        for target in &targets.targets {
            record_match(ObservedCallTarget::Resolved(*target));
        }
        for target in &targets.known_targets {
            record_match(ObservedCallTarget::Known(*target));
        }
        for provenance in &module_provenance {
            record_match(ObservedCallTarget::ModuleProvenance(provenance));
        }
        if targets.has_unresolved
            || (targets.targets.is_empty()
                && targets.known_targets.is_empty()
                && module_provenance.is_empty())
        {
            record_match(ObservedCallTarget::Deferred);
        }
        self.calls.push(CallFact {
            caller,
            range: call.range,
            targets: targets.targets,
            no_matches: no_matches.into_boxed_slice(),
            unsupported_function_statement_suppression: self.statement_suppression(statement),
            unsupported_function_caller_suppression: self.function_suppression(caller),
        });
    }
}

impl<'a> SourceOrderVisitor<'a> for FactVisitor<'a, '_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        let enclosing_statement = self.statement.replace(statement.range());
        match statement {
            ast::Stmt::FunctionDef(function) => self.visit_function(function),
            ast::Stmt::ClassDef(class) => {
                let enclosing_caller = self.caller.take();
                self.visit_body(&class.body);
                self.caller = enclosing_caller;
            }
            ast::Stmt::TypeAlias(_) => {}
            _ => walk_stmt(self, statement),
        }
        self.statement = enclosing_statement;
    }

    fn visit_annotation(&mut self, _annotation: &'a ast::Expr) {}

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if matches!(
            expression,
            ast::Expr::Lambda(_)
                | ast::Expr::ListComp(_)
                | ast::Expr::SetComp(_)
                | ast::Expr::DictComp(_)
                | ast::Expr::Generator(_)
        ) {
            return;
        }

        if let ast::Expr::Call(call) = expression {
            self.visit_call(call);
        }
        walk_expr(self, expression);
    }
}

fn is_baked_resolver_origin(origin: ModuleOrigin, module: &str, symbol: &str) -> bool {
    if origin != ModuleOrigin::ThirdParty {
        return false;
    }

    match module {
        "chalk.features.resolver" | "chalk.features" => {
            matches!(symbol, "online" | "offline" | "resolver")
        }
        "chalk" => matches!(
            symbol,
            "online" | "offline" | "resolver" | "batch" | "realtime"
        ),
        _ => false,
    }
}
