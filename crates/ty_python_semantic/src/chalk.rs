//! Semantic facts needed by Chalk-specific analysis.
//!
//! This module intentionally exposes only facts that require ty's semantic index or native
//! types. Chalk policy and persistent indexing belong in downstream crates.

use ruff_db::files::{File, FileRange};
use ruff_db::parsed::parsed_module;
use ruff_python_ast::{self as ast, name::Name};
use ruff_text_size::{Ranged, TextRange};
use ty_module_resolver::{Module, ModuleName, file_to_module, resolve_module, resolve_real_module};
use ty_python_core::ast_ids::HasScopedUseId;
pub use ty_python_core::definition::Definition;
use ty_python_core::definition::{DefinitionKind, DefinitionState};
use ty_python_core::scope::NodeWithScopeKind;
use ty_python_core::semantic_index;

use crate::place::{Definedness, Place};
use crate::types::{KnownBoundMethodType, Type, TypeDefinition, binding_type};
use crate::{
    HasType, ImportAliasResolution, SemanticModel, definitions_for_attribute, definitions_for_name,
};

/// A bounded structural view of a native ty type for downstream Chalk analysis.
///
/// The view preserves native [`Type`] identities for recursive components. It never formats or
/// reparses types.
#[derive(Debug)]
pub enum ChalkTypeShape<'db> {
    Dynamic,
    Unavailable,
    Never,
    Expanded(Type<'db>),
    Union(&'db [Type<'db>]),
    Intersection(ChalkIntersection<'db>),
    Concrete,
}
/// Native components of an intersection, together with ty's materialized bounds.
#[derive(Debug)]
pub struct ChalkIntersection<'db> {
    pub positive: Box<[Type<'db>]>,
    pub top_materialization: Type<'db>,
    pub bottom_materialization: Type<'db>,
}

/// A native container category requested by Chalk's registry matcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChalkContainerKind {
    List,
    Set,
    FrozenSet,
    Dict,
    Tuple,
    Iterable,
    Generator,
    Counter,
}

/// Native type arguments for a container category.
#[derive(Debug)]
pub enum ChalkContainerType<'db> {
    NotContainer,
    Unavailable,
    Unary(Type<'db>),
    Mapping { key: Type<'db>, value: Type<'db> },
    Tuple(ChalkTupleType<'db>),
}

/// A native fixed- or variable-length tuple specification.
#[derive(Debug)]
pub struct ChalkTupleType<'db> {
    pub elements: Box<[Type<'db>]>,
    pub is_variable: bool,
}

/// The result of a semantic class-identity or ancestry query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChalkClassRelation {
    Match,
    NoMatch,
    Unavailable,
}

/// Returns a structural view of `ty` without retaining it in a Salsa-cached Chalk result.
pub fn chalk_type_shape<'db>(db: &'db dyn crate::Db, ty: Type<'db>) -> ChalkTypeShape<'db> {
    ty.chalk_type_shape(db)
}

/// Returns native type arguments when `ty` belongs to the requested container category.
pub fn chalk_container_type<'db>(
    db: &'db dyn crate::Db,
    ty: Type<'db>,
    kind: ChalkContainerKind,
) -> ChalkContainerType<'db> {
    ty.chalk_container_type(db, kind)
}

/// Tests the exact semantic identity of a native instance or literal's runtime class.
pub fn chalk_exact_instance_class(
    db: &dyn crate::Db,
    ty: Type<'_>,
    module: &str,
    name: &str,
) -> ChalkClassRelation {
    ty.chalk_exact_instance_class(db, module, name)
}

/// Tests the exact semantic identity of a native class object.
pub fn chalk_exact_class_object(
    db: &dyn crate::Db,
    ty: Type<'_>,
    module: &str,
    name: &str,
) -> ChalkClassRelation {
    ty.chalk_exact_class_object(db, module, name)
}

/// Tests whether a native instance or class object semantically derives from a qualified class.
pub fn chalk_class_derived_from(
    db: &dyn crate::Db,
    ty: Type<'_>,
    module: &str,
    name: &str,
) -> ChalkClassRelation {
    ty.chalk_class_derived_from(db, module, name)
}

/// Tests whether a native instance semantically derives from a qualified class.
pub fn chalk_instance_derived_from(
    db: &dyn crate::Db,
    ty: Type<'_>,
    module: &str,
    name: &str,
) -> ChalkClassRelation {
    ty.chalk_instance_derived_from(db, module, name)
}

/// Returns whether `ty` is the exact module literal named by `expected`.
pub fn chalk_module_is(db: &dyn crate::Db, ty: Type<'_>, expected: &str) -> bool {
    ty.chalk_module_is(db, expected)
}

/// Returns whether `ty` has native enum semantics, for either an instance or class object.
pub fn chalk_is_enum(db: &dyn crate::Db, ty: Type<'_>) -> ChalkClassRelation {
    ty.chalk_is_enum(db)
}

/// Returns whether `ty` has a native structural shape that Chalk can treat as a logical struct.
pub fn chalk_is_logical_struct(db: &dyn crate::Db, ty: Type<'_>) -> bool {
    ty.chalk_is_logical_struct(db)
}

/// The kind of statically resolved definition targeted by a call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum CallTargetKind {
    Function,
    ClassConstructor,
}

/// A statically resolved definition targeted by a call.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub struct CallTarget<'db> {
    pub definition: Definition<'db>,
    pub kind: CallTargetKind,
}

/// The statically possible definitions targeted by a call.
#[derive(Debug)]
pub struct CallTargets<'db> {
    /// Every inferred function or static-class constructor target, plus exact source definitions
    /// used as a conservative fallback when inference produced no target.
    pub targets: Box<[CallTarget<'db>]>,
    /// Compact targets for built-in operations that have no native [`Definition`].
    pub known_targets: Box<[KnownCallTarget]>,
    /// Whether at least one possible target could not be represented by [`CallTarget`].
    pub has_unresolved: bool,
}

/// A statically known call target without a native [`Definition`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum KnownCallTarget {
    StrStartswith,
}

/// The semantic search-path origin of a resolved module.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum ModuleOrigin {
    StandardLibrary,
    ThirdParty,
    FirstParty,
    Extra,
    Namespace,
    Unresolved,
    Other,
}

/// Classifies a resolved module by search-path identity rather than normalized module-name text.
pub(crate) fn module_origin(db: &dyn crate::Db, module: Option<Module<'_>>) -> ModuleOrigin {
    let Some(module) = module else {
        return ModuleOrigin::Unresolved;
    };
    let Some(search_path) = module.search_path(db) else {
        return ModuleOrigin::Namespace;
    };
    if search_path.is_standard_library() {
        ModuleOrigin::StandardLibrary
    } else if search_path.is_third_party() {
        ModuleOrigin::ThirdParty
    } else if search_path.is_first_party() {
        ModuleOrigin::FirstParty
    } else if search_path.is_extra() {
        ModuleOrigin::Extra
    } else {
        ModuleOrigin::Other
    }
}

/// Classifies module ownership using the runtime module when one exists.
///
/// A concrete or namespace runtime module overrides typing-only stubs. If no runtime module
/// exists, a canonical vendored stub retains its typing origin.
pub fn module_ownership_origin(
    db: &dyn crate::Db,
    importing_file: File,
    module_name: &ModuleName,
    typing_module: Option<Module<'_>>,
) -> ModuleOrigin {
    if let Some(runtime_module) = resolve_real_module(db, importing_file, module_name) {
        return module_origin(db, Some(runtime_module));
    }

    typing_module
        .filter(|module| {
            module
                .file(db)
                .is_some_and(|file| file.path(db).is_vendored_path())
        })
        .map_or(ModuleOrigin::Unresolved, |module| {
            module_origin(db, Some(module))
        })
}

/// A semantically established module and symbol targeted by a call expression.
///
/// This is independent of whether native inference can materialize a callable [`Definition`].
#[derive(Clone, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub struct CallModuleProvenance {
    module: Box<str>,
    symbol: Name,
    origin: ModuleOrigin,
    ownership_origin: ModuleOrigin,
    receiver_parameter: Option<u32>,
}

impl CallModuleProvenance {
    pub fn module_name(&self) -> &str {
        &self.module
    }

    pub fn symbol_name(&self) -> &str {
        self.symbol.as_str()
    }

    /// The typing-resolved module's origin.
    pub const fn origin(&self) -> ModuleOrigin {
        self.origin
    }

    /// The runtime module's origin, retaining canonical vendored typing stubs as a fallback.
    pub const fn ownership_origin(&self) -> ModuleOrigin {
        self.ownership_origin
    }

    pub const fn receiver_parameter(&self) -> Option<u32> {
        self.receiver_parameter
    }
}

/// Semantically established module provenance for a decorator symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecoratorModuleProvenance {
    module: ModuleName,
    symbol: Name,
    origin: ModuleOrigin,
    ownership_origin: ModuleOrigin,
}

impl DecoratorModuleProvenance {
    /// The normalized absolute module name.
    pub fn module_name(&self) -> &str {
        self.module.as_ref()
    }

    /// The symbol name in the originating module, before any local alias.
    pub fn symbol_name(&self) -> &str {
        self.symbol.as_str()
    }

    /// The typing definition's module origin.
    pub const fn origin(&self) -> ModuleOrigin {
        self.origin
    }

    /// The runtime module's origin, retaining canonical vendored typing stubs as a fallback.
    pub const fn ownership_origin(&self) -> ModuleOrigin {
        self.ownership_origin
    }
}

/// The defining module and symbol of a resolved decorator function.
#[derive(Clone, Debug, Eq, PartialEq, salsa::Update, get_size2::GetSize)]
pub struct FunctionDefinitionOrigin {
    module: Box<str>,
    symbol: Name,
    origin: ModuleOrigin,
    ownership_origin: ModuleOrigin,
}

impl FunctionDefinitionOrigin {
    /// The normalized absolute name of the module containing the definition.
    pub fn module_name(&self) -> &str {
        &self.module
    }

    /// The name of the top-level function in its defining module.
    pub fn symbol_name(&self) -> &str {
        self.symbol.as_str()
    }

    /// The typing definition's module origin.
    pub const fn origin(&self) -> ModuleOrigin {
        self.origin
    }

    /// The runtime module's origin, retaining canonical vendored typing stubs as a fallback.
    pub const fn ownership_origin(&self) -> ModuleOrigin {
        self.ownership_origin
    }
}

pub type DecoratorDefinitionOrigin = FunctionDefinitionOrigin;

/// The semantic kind of a resolved call definition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub enum CallDefinitionOriginKind {
    TopLevelFunction,
    Method,
    NestedFunction,
    ClassConstructor,
}

/// The defining module, symbol, and scope kind of a resolved call definition.
#[derive(Clone, Debug, Eq, Hash, PartialEq, salsa::Update, get_size2::GetSize)]
pub struct CallDefinitionOrigin {
    module: Box<str>,
    symbol: Name,
    qualified_symbol: Box<str>,
    definition_file: File,
    definition_range: TextRange,
    kind: CallDefinitionOriginKind,
    origin: ModuleOrigin,
    ownership_origin: ModuleOrigin,
}

impl CallDefinitionOrigin {
    /// The normalized absolute name of the defining module.
    pub fn module_name(&self) -> &str {
        &self.module
    }

    /// The symbol name at the definition.
    pub fn symbol_name(&self) -> &str {
        self.symbol.as_str()
    }

    /// The definition's qualified symbol, excluding its module.
    pub fn qualified_symbol(&self) -> &str {
        &self.qualified_symbol
    }

    /// The exact range of the definition's name.
    pub const fn definition_range(&self) -> FileRange {
        FileRange::new(self.definition_file, self.definition_range)
    }

    pub const fn kind(&self) -> CallDefinitionOriginKind {
        self.kind
    }

    pub const fn origin(&self) -> ModuleOrigin {
        self.origin
    }

    /// The runtime module's origin, retaining canonical vendored typing stubs as a fallback.
    pub const fn ownership_origin(&self) -> ModuleOrigin {
        self.ownership_origin
    }
}

/// Semantic provenance for a decorator expression.
#[derive(Debug)]
pub struct DecoratorProvenance<'db> {
    /// Every decorator function definition that ty resolved.
    pub definitions: Box<[Definition<'db>]>,
    /// All module origins established from direct imports or module-literal receivers.
    pub modules: Box<[DecoratorModuleProvenance]>,
    /// Whether module provenance accounts for every possible decorator target.
    pub module_fallback_is_complete: bool,
    /// Whether at least one possible decorator target could not be represented by these facts.
    pub has_unresolved: bool,
}

impl<'db> SemanticModel<'db> {
    /// Returns all statically possible function and static-class constructor definitions called by
    /// `call`.
    ///
    /// This includes direct, imported, aliased, module-qualified, static, class, and bound method
    /// calls, including unions of those forms. Unsupported callable forms and dynamic alternatives
    /// set [`CallTargets::has_unresolved`] instead of erasing resolved alternatives.
    pub fn chalk_call_targets(&self, call: &ast::ExprCall) -> CallTargets<'db> {
        call_targets(self, &call.func)
    }

    /// Returns semantic module provenance for the callee, even when it has no native definition.
    pub fn chalk_call_module_provenance(
        &self,
        call: &ast::ExprCall,
    ) -> Box<[CallModuleProvenance]> {
        call_module_provenance(self, &call.func).into_boxed_slice()
    }

    /// Returns semantic provenance for a decorator or decorator factory.
    ///
    /// For a factory such as `@decorator(...)`, provenance describes `decorator`.
    pub fn chalk_decorator_provenance(
        &self,
        decorator: &ast::Decorator,
    ) -> DecoratorProvenance<'db> {
        let target = decorator
            .expression
            .as_call_expr()
            .map_or(&decorator.expression, |call| &call.func);

        let targets = call_targets(self, target);
        let mut definitions = Vec::new();
        let mut has_unresolved = targets.has_unresolved;
        for target in targets.targets {
            match target.kind {
                CallTargetKind::Function => definitions.push(target.definition),
                CallTargetKind::ClassConstructor => has_unresolved = true,
            }
        }
        let (modules, module_fallback_is_complete) = decorator_module_provenance(self, target);
        DecoratorProvenance {
            definitions: definitions.into_boxed_slice(),
            modules: modules.into_boxed_slice(),
            module_fallback_is_complete,
            has_unresolved,
        }
    }

    /// Returns the exact defining module and symbol for a resolved top-level decorator function.
    ///
    /// This deliberately rejects methods and nested functions. Downstream Chalk policy can use
    /// the result to recognize known baked decorators without depending on ty's definition
    /// internals.
    pub fn chalk_decorator_definition_origin(
        &self,
        definition: Definition<'db>,
    ) -> Option<&'db DecoratorDefinitionOrigin> {
        chalk_function_definition_origin(self.db(), definition)
    }
}

/// Returns the defining module and symbol of a top-level function definition.
///
/// This tracked helper isolates the definition file's AST dependency from callers in other files.
#[salsa::tracked(
    returns(as_ref),
    heap_size=ruff_memory_usage::heap_size
)]
pub fn chalk_function_definition_origin<'db>(
    db: &'db dyn crate::Db,
    definition: Definition<'db>,
) -> Option<FunctionDefinitionOrigin> {
    if !matches!(definition.scope(db).node(db), NodeWithScopeKind::Module) {
        return None;
    }

    let DefinitionKind::Function(function) = definition.kind(db) else {
        return None;
    };
    let module = file_to_module(db, definition.file(db));
    let origin = module.map_or(ModuleOrigin::Other, |module| {
        module_origin(db, Some(module))
    });
    let ownership_origin = module.map_or(ModuleOrigin::Other, |module| {
        module_ownership_origin(db, definition.file(db), module.name(db), Some(module))
    });
    let parsed = parsed_module(db, definition.file(db)).load(db);

    Some(FunctionDefinitionOrigin {
        module: module.map_or("", |module| module.name(db).as_str()).into(),
        symbol: Name::new(function.node(&parsed).name.as_str()),
        origin,
        ownership_origin,
    })
}

/// Returns the semantic origin of a function or class definition targeted by a call.
///
/// This tracked helper isolates the definition file's AST dependency from callers in other files.
#[salsa::tracked(
    returns(as_ref),
    heap_size=ruff_memory_usage::heap_size
)]
pub fn chalk_call_definition_origin<'db>(
    db: &'db dyn crate::Db,
    definition: Definition<'db>,
) -> Option<CallDefinitionOrigin> {
    let module = file_to_module(db, definition.file(db));
    let origin = module.map_or(ModuleOrigin::Other, |module| {
        module_origin(db, Some(module))
    });
    let ownership_origin = module.map_or(ModuleOrigin::Other, |module| {
        module_ownership_origin(db, definition.file(db), module.name(db), Some(module))
    });
    let parsed = parsed_module(db, definition.file(db)).load(db);

    let (symbol, kind) = match definition.kind(db) {
        DefinitionKind::Function(function) => {
            let kind = match definition.scope(db).node(db) {
                NodeWithScopeKind::Module => CallDefinitionOriginKind::TopLevelFunction,
                NodeWithScopeKind::Class(_) => CallDefinitionOriginKind::Method,
                _ => CallDefinitionOriginKind::NestedFunction,
            };
            (Name::new(function.node(&parsed).name.as_str()), kind)
        }
        DefinitionKind::Class(class) => (
            Name::new(class.node(&parsed).name.as_str()),
            CallDefinitionOriginKind::ClassConstructor,
        ),
        _ => return None,
    };
    let qualified_symbol =
        call_definition_qualified_symbol(db, definition, &parsed, symbol.as_str());
    let definition_range = definition.focus_range(db, &parsed);

    Some(CallDefinitionOrigin {
        module: module.map_or("", |module| module.name(db).as_str()).into(),
        symbol,
        qualified_symbol,
        definition_file: definition_range.file(),
        definition_range: definition_range.range(),
        kind,
        origin,
        ownership_origin,
    })
}

fn call_definition_qualified_symbol(
    db: &dyn crate::Db,
    definition: Definition<'_>,
    parsed: &ruff_db::parsed::ParsedModuleRef,
    symbol: &str,
) -> Box<str> {
    let index = semantic_index(db, definition.file(db));
    let mut components = Vec::new();
    for (_, scope) in index.ancestor_scopes(definition.file_scope(db)) {
        match scope.node() {
            NodeWithScopeKind::Class(class) => {
                components.push(class.node(parsed).name.as_str().to_owned());
            }
            NodeWithScopeKind::Function(function) => {
                components.push(format!("{}.<locals>", function.node(parsed).name.as_str()));
            }
            _ => {}
        }
    }
    components.reverse();
    components.push(symbol.to_owned());
    components.join(".").into()
}

/// Relates a module, class, or instance receiver to a registry-qualified owner.
pub fn chalk_receiver_module_relation(
    db: &dyn crate::Db,
    ty: Type<'_>,
    expected: &str,
) -> ChalkClassRelation {
    ty.chalk_receiver_module_relation(db, expected)
}

#[derive(Default)]
struct CallTargetCollector<'db> {
    targets: Vec<CallTarget<'db>>,
    known_targets: Vec<KnownCallTarget>,
    has_unresolved: bool,
    saw_never: bool,
}

impl<'db> CallTargetCollector<'db> {
    fn collect_expression(&mut self, model: &SemanticModel<'db>, expression: &ast::Expr) {
        match expression {
            ast::Expr::Name(name) => self.collect_name(model, name),
            ast::Expr::Attribute(attribute) => self.collect_attribute(model, attribute),
            _ => match expression.inferred_type(model) {
                Some(ty) => self.collect_type(model, ty),
                None => self.has_unresolved = true,
            },
        }
    }

    fn collect_name(&mut self, model: &SemanticModel<'db>, name: &ast::ExprName) {
        let db = model.db();
        let file = model.file();
        let index = semantic_index(db, file);
        let name_ref = ast::ExprRef::Name(name);
        let scope = index.expression_scope_id(&name_ref);
        let use_def = index.use_def_map(scope);
        let target_count = self.targets.len();
        let known_target_count = self.known_targets.len();
        let mut has_definition = false;
        let mut has_missing_binding = false;

        for binding in use_def.bindings_at_use(name.scoped_use_id(db, file)) {
            match binding.binding {
                DefinitionState::Defined(definition) => {
                    has_definition = true;
                    self.collect_type(model, binding_type(db, definition));
                }
                DefinitionState::Undefined | DefinitionState::Deleted => {
                    has_missing_binding = true;
                }
            }
        }

        if has_definition {
            self.has_unresolved |= has_missing_binding;
        } else {
            match name.inferred_type(model) {
                Some(ty) => self.collect_type(model, ty),
                None => self.has_unresolved = true,
            }
        }

        if self.targets.len() == target_count && self.known_targets.len() == known_target_count {
            for resolved in definitions_for_name(
                model,
                name.id.as_str(),
                name.into(),
                ImportAliasResolution::ResolveAliases,
            ) {
                if let Some(definition) = resolved.definition() {
                    self.collect_source_definition(model, definition);
                }
            }
        }
    }

    fn collect_attribute(&mut self, model: &SemanticModel<'db>, attribute: &ast::ExprAttribute) {
        let target_count = self.targets.len();
        let known_target_count = self.known_targets.len();
        let Some(receiver) = attribute.value.inferred_type(model) else {
            self.has_unresolved = true;
            return;
        };
        self.collect_attribute_on_type(model, receiver, attribute.attr.as_str());

        if self.targets.len() == target_count && self.known_targets.len() == known_target_count {
            for resolved in definitions_for_attribute(model, attribute) {
                if let Some(definition) = resolved.definition() {
                    self.collect_source_definition(model, definition);
                }
            }
        }
    }

    fn collect_attribute_on_type(
        &mut self,
        model: &SemanticModel<'db>,
        receiver: Type<'db>,
        name: &str,
    ) {
        match receiver {
            Type::Union(union) => {
                for element in union.elements(model.db()) {
                    self.collect_attribute_on_type(model, *element, name);
                }
            }
            receiver => match receiver.member(model.db(), name).place {
                Place::Defined(member) => {
                    self.has_unresolved |= member.definedness == Definedness::PossiblyUndefined;
                    self.collect_type(model, member.ty);
                }
                Place::Undefined => self.has_unresolved = true,
            },
        }
    }

    fn collect_type(&mut self, model: &SemanticModel<'db>, ty: Type<'db>) {
        match ty {
            Type::FunctionLiteral(_) | Type::BoundMethod(_) => match ty.definition(model.db()) {
                Some(TypeDefinition::Function(definition)) => {
                    self.push(definition, CallTargetKind::Function);
                }
                _ => self.has_unresolved = true,
            },
            Type::ClassLiteral(_) => match ty.definition(model.db()) {
                Some(TypeDefinition::StaticClass(definition)) => {
                    self.push(definition, CallTargetKind::ClassConstructor);
                }
                _ => self.has_unresolved = true,
            },
            Type::KnownBoundMethod(KnownBoundMethodType::StrStartswith(_)) => {
                self.push_known(KnownCallTarget::StrStartswith);
            }
            Type::Union(union) => {
                for element in union.elements(model.db()) {
                    self.collect_type(model, *element);
                }
            }
            Type::Never => self.saw_never = true,
            _ => self.has_unresolved = true,
        }
    }

    fn push(&mut self, definition: Definition<'db>, kind: CallTargetKind) {
        let target = CallTarget { definition, kind };
        if !self.targets.contains(&target) {
            self.targets.push(target);
        }
    }

    fn push_known(&mut self, target: KnownCallTarget) {
        if !self.known_targets.contains(&target) {
            self.known_targets.push(target);
        }
    }

    fn collect_source_definition(
        &mut self,
        model: &SemanticModel<'db>,
        definition: Definition<'db>,
    ) {
        match definition.kind(model.db()) {
            DefinitionKind::Function(_) => {
                self.has_unresolved = true;
                self.push(definition, CallTargetKind::Function);
            }
            DefinitionKind::Class(_) => {
                self.has_unresolved = true;
                self.push(definition, CallTargetKind::ClassConstructor);
            }
            _ => {}
        }
    }
}

fn call_targets<'db>(model: &SemanticModel<'db>, expression: &ast::Expr) -> CallTargets<'db> {
    let mut collector = CallTargetCollector::default();
    collector.collect_expression(model, expression);
    let has_unresolved =
        collector.has_unresolved || (collector.saw_never && collector.targets.is_empty());
    CallTargets {
        targets: collector.targets.into_boxed_slice(),
        known_targets: collector.known_targets.into_boxed_slice(),
        has_unresolved,
    }
}

fn call_module_provenance(
    model: &SemanticModel<'_>,
    expression: &ast::Expr,
) -> Vec<CallModuleProvenance> {
    let targets = call_targets(model, expression);
    if !targets.targets.is_empty() || !targets.known_targets.is_empty() {
        return Vec::new();
    }

    let mut provenance = Vec::new();
    match expression {
        ast::Expr::Name(name) => {
            let mut imported = Vec::new();
            let _ = directly_imported_symbols(model, name, &mut imported);
            provenance.extend(imported.into_iter().filter_map(|origin| {
                (origin.ownership_origin == ModuleOrigin::ThirdParty
                    && is_chalk_namespace(origin.module.as_str()))
                .then(|| CallModuleProvenance {
                    module: origin.module.as_str().into(),
                    symbol: origin.symbol,
                    origin: origin.origin,
                    ownership_origin: origin.ownership_origin,
                    receiver_parameter: None,
                })
            }));
        }
        ast::Expr::Attribute(attribute) => {
            if let Some(receiver) = attribute.value.inferred_type(model) {
                receiver_call_module_provenance(
                    model,
                    receiver,
                    attribute.attr.as_str(),
                    &mut provenance,
                );
            }
        }
        _ => {}
    }
    provenance
}

fn receiver_call_module_provenance(
    model: &SemanticModel<'_>,
    receiver: Type<'_>,
    symbol: &str,
    provenance: &mut Vec<CallModuleProvenance>,
) {
    if let Type::Union(union) = receiver {
        for element in union.elements(model.db()) {
            receiver_call_module_provenance(model, *element, symbol, provenance);
        }
        return;
    }

    let (module, receiver_parameter) = if let Type::ModuleLiteral(module) = receiver {
        (module.module(model.db()), None)
    } else if let Some(module) = receiver.chalk_receiver_module(model.db()) {
        (module, Some(0))
    } else {
        return;
    };
    let origin = module_origin(model.db(), Some(module));
    let ownership_origin = module_ownership_origin(
        model.db(),
        model.file(),
        module.name(model.db()),
        Some(module),
    );
    if ownership_origin != ModuleOrigin::ThirdParty
        || !is_chalk_namespace(module.name(model.db()).as_str())
    {
        return;
    }
    let candidate = CallModuleProvenance {
        module: module.name(model.db()).as_str().into(),
        symbol: Name::new(symbol),
        origin,
        ownership_origin,
        receiver_parameter,
    };
    if !provenance.contains(&candidate) {
        provenance.push(candidate);
    }
}

fn is_chalk_namespace(module: &str) -> bool {
    ["chalk", "chalkdf"].into_iter().any(|namespace| {
        module == namespace
            || module
                .strip_prefix(namespace)
                .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

fn decorator_module_provenance(
    model: &SemanticModel<'_>,
    expression: &ast::Expr,
) -> (Vec<DecoratorModuleProvenance>, bool) {
    let mut provenance = Vec::new();
    let complete = match expression {
        ast::Expr::Name(name) => directly_imported_symbols(model, name, &mut provenance),
        ast::Expr::Attribute(attribute) => {
            if let Some(receiver) = attribute.value.inferred_type(model) {
                module_attribute_provenance(
                    model,
                    receiver,
                    attribute.attr.as_str(),
                    &mut provenance,
                )
            } else {
                false
            }
        }
        _ => false,
    };
    (provenance, complete)
}

fn directly_imported_symbols(
    model: &SemanticModel<'_>,
    name: &ast::ExprName,
    provenance: &mut Vec<DecoratorModuleProvenance>,
) -> bool {
    let db = model.db();
    let file = model.file();
    let index = semantic_index(db, file);
    let name_ref = ast::ExprRef::Name(name);
    let scope = index.expression_scope_id(&name_ref);
    let use_def = index.use_def_map(scope);
    let mut saw_binding = false;
    let mut complete = true;

    for binding in use_def.bindings_at_use(name.scoped_use_id(db, file)) {
        saw_binding = true;
        let Some(definition) = binding.binding.definition() else {
            complete = false;
            continue;
        };
        let module = parsed_module(db, definition.file(db)).load(db);
        let candidate = match definition.kind(db) {
            DefinitionKind::ImportFrom(import) => {
                let statement = import.import(&module);
                let Ok(module_name) =
                    ModuleName::from_import_statement(db, definition.file(db), statement)
                else {
                    complete = false;
                    continue;
                };
                let symbol = Name::new(import.alias(&module).name.as_str());
                let typing_module = resolve_module(db, definition.file(db), &module_name);
                let origin = module_origin(db, typing_module);
                let ownership_origin =
                    module_ownership_origin(db, definition.file(db), &module_name, typing_module);
                DecoratorModuleProvenance {
                    module: module_name,
                    symbol,
                    origin,
                    ownership_origin,
                }
            }
            DefinitionKind::StarImport(import) => {
                let statement = import.import(&module);
                let Ok(module_name) =
                    ModuleName::from_import_statement(db, definition.file(db), statement)
                else {
                    complete = false;
                    continue;
                };
                let typing_module = resolve_module(db, definition.file(db), &module_name);
                let origin = module_origin(db, typing_module);
                let ownership_origin =
                    module_ownership_origin(db, definition.file(db), &module_name, typing_module);
                DecoratorModuleProvenance {
                    module: module_name,
                    symbol: name.id.clone(),
                    origin,
                    ownership_origin,
                }
            }
            _ => {
                complete = false;
                continue;
            }
        };

        if !provenance.contains(&candidate) {
            provenance.push(candidate);
        }
    }

    saw_binding && complete
}

fn module_attribute_provenance(
    model: &SemanticModel<'_>,
    receiver: Type<'_>,
    symbol: &str,
    provenance: &mut Vec<DecoratorModuleProvenance>,
) -> bool {
    match receiver {
        Type::Union(union) => {
            let mut complete = true;
            for element in union.elements(model.db()) {
                complete &= module_attribute_provenance(model, *element, symbol, provenance);
            }
            complete
        }
        Type::ModuleLiteral(module) => {
            let module = module.module(model.db());
            let candidate = DecoratorModuleProvenance {
                module: module.name(model.db()).clone(),
                symbol: Name::new(symbol),
                origin: module_origin(model.db(), Some(module)),
                ownership_origin: module_ownership_origin(
                    model.db(),
                    model.file(),
                    module.name(model.db()),
                    Some(module),
                ),
            };
            if !provenance.contains(&candidate) {
                provenance.push(candidate);
            }
            true
        }
        _ => false,
    }
}
