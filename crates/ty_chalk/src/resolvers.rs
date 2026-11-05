//! Resolver discovery shared by diagnostics and navigation.

use ruff_db::files::File;
use ruff_db::parsed::parsed_module;
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_stmt};
use ruff_python_ast::{self as ast};
use ruff_text_size::{Ranged, TextRange};
use ty_python_semantic::chalk::ModuleOrigin;
use ty_python_semantic::{Db, SemanticModel};

/// Returns source-ordered resolver function ranges without checking function bodies or
/// extracting diagnostics facts. The cached ranges retain no AST nodes.
#[salsa::tracked(returns(deref), heap_size=ruff_memory_usage::heap_size)]
pub(crate) fn resolver_function_ranges(db: &dyn Db, file: File) -> Box<[TextRange]> {
    let parsed = parsed_module(db, file).load(db);
    let model = SemanticModel::new(db, file);
    let mut visitor = ResolverVisitor {
        model: &model,
        ranges: Vec::new(),
    };
    visitor.visit_body(&parsed.syntax().body);
    visitor.ranges.into_boxed_slice()
}

struct ResolverVisitor<'a, 'db> {
    model: &'a SemanticModel<'db>,
    ranges: Vec<TextRange>,
}

impl ResolverVisitor<'_, '_> {
    fn is_resolver_decorator(&self, decorator: &ast::Decorator) -> bool {
        let provenance = self.model.chalk_decorator_provenance(decorator);

        if !provenance.definitions.is_empty() {
            !provenance.has_unresolved
                && provenance.definitions.iter().all(|definition| {
                    self.model
                        .chalk_decorator_definition_origin(*definition)
                        .is_some_and(|origin| {
                            is_baked_resolver_origin(
                                origin.ownership_origin,
                                origin.module.as_ref(),
                                origin.symbol.as_str(),
                            )
                        })
                })
        } else {
            provenance.module_fallback_is_complete
                && !provenance.modules.is_empty()
                && provenance.modules.iter().all(|origin| {
                    is_baked_resolver_origin(
                        origin.ownership_origin,
                        origin.module.as_ref(),
                        origin.symbol.as_str(),
                    )
                })
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for ResolverVisitor<'_, '_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement
            && function
                .decorator_list
                .iter()
                .any(|decorator| self.is_resolver_decorator(decorator))
        {
            self.ranges.push(function.range());
        }
        walk_stmt(self, statement);
    }

    // Still descend into statements to find nested resolvers, but skip body expressions,
    // annotations, defaults, and decorators; only resolver provenance needs inference.
    fn visit_expr(&mut self, _expression: &'a ast::Expr) {}
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

#[cfg(test)]
mod tests {
    use super::{ModuleOrigin, is_baked_resolver_origin};

    #[test]
    fn rejects_non_third_party_origins() {
        for origin in [
            ModuleOrigin::StandardLibrary,
            ModuleOrigin::FirstParty,
            ModuleOrigin::Extra,
            ModuleOrigin::Namespace,
            ModuleOrigin::Unresolved,
            ModuleOrigin::Other,
        ] {
            assert!(!is_baked_resolver_origin(origin, "chalk", "online"));
        }
    }
}
