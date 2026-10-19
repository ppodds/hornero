//! Bootstrap-time validation: walks the module tree, applies visibility rules, resolves every
//! dependency edge to a concrete node, detects cycles and computes effective scopes.
//!
//! Nothing here constructs a provider, so [`Hornero::validate`](crate::Hornero::validate) can run it in a
//! plain `#[test]`.

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use axum::Router;

use super::injectable::Scope;
use super::key::Key;
use super::module::{Identity, LayerFn, Module, ModuleDef, ModuleKey};
use super::provider::{Builtin, ProviderDef};
use super::runtime::{RtNode, Runtime};
use crate::app::join_paths;
use crate::error::{BootError, Problem};
use crate::lifecycle::Shutdown;
use crate::util::short_type_name;
use crate::web::controller::ControllerEntry;
use crate::web::enhancer::{EnhancerKind, EnhancerRef};
use crate::web::routes::{EnhancerInfo, RouteInfo, Routes};

#[derive(Clone, Default)]
pub(crate) struct GlobalEnhancers {
    pub(crate) guards: Vec<EnhancerRef>,
    pub(crate) interceptors: Vec<EnhancerRef>,
    pub(crate) filters: Vec<EnhancerRef>,
    pub(crate) middlewares: Vec<EnhancerRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Provider,
    Controller,
    Slot(EnhancerKind),
    Mount,
    Builtin,
}

pub(crate) struct Node {
    pub(crate) def: ProviderDef,
    pub(crate) module: usize,
    pub(crate) kind: NodeKind,
    pub(crate) label: String,
    pub(crate) deps: Vec<usize>,
    /// Declared optional dependencies that nothing visible provides.
    pub(crate) absent: Vec<Key>,
    /// Edges were set when the node was created (enhancer slots) and skip normal resolution.
    preset: bool,
}

pub(crate) struct ModuleNode {
    type_id: TypeId,
    /// [`Module::name`], numbered after collection when several modules share it (see
    /// [`Graph::label_instances`]).
    pub(crate) name: String,
    /// [`Module::name`] as returned.
    base_name: String,
    /// The module whose import created this one; `None` for the root and the core module.
    importer: Option<usize>,
    global: bool,
    imports: Vec<usize>,
    local: HashMap<Key, usize>,
    implicit: HashMap<Key, usize>,
    pub(crate) slots: HashMap<Key, usize>,
    exports: Vec<Key>,
    export_modules: Vec<(TypeId, &'static str)>,
    exported: HashMap<Key, usize>,
    pub(crate) controllers: Vec<(usize, ControllerEntry)>,
    pub(crate) middlewares: Vec<EnhancerRef>,
    pub(crate) layers: Vec<LayerFn>,
    /// Path, node and (for `mount_router`) the router itself.
    pub(crate) mounts: Vec<(String, usize, Option<Router>)>,
}

impl ModuleNode {
    fn new(type_id: TypeId, name: String, global: bool, importer: Option<usize>) -> Self {
        Self {
            type_id,
            base_name: name.clone(),
            name,
            importer,
            global,
            imports: Vec::new(),
            local: HashMap::new(),
            implicit: HashMap::new(),
            slots: HashMap::new(),
            exports: Vec::new(),
            export_modules: Vec::new(),
            exported: HashMap::new(),
            controllers: Vec::new(),
            middlewares: Vec::new(),
            layers: Vec::new(),
            mounts: Vec::new(),
        }
    }
}

struct Override {
    key: Key,
    def: ProviderDef,
    used: bool,
}

pub(crate) enum Lookup {
    Found(usize),
    Missing,
    Ambiguous(Vec<usize>),
}

pub(crate) struct Graph {
    pub(crate) modules: Vec<ModuleNode>,
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: usize,
    /// Topological order: dependencies before dependents.
    pub(crate) order: Vec<usize>,
    pub(crate) effective: Vec<Scope>,
    pub(crate) report: ScopeReport,
    /// Global enhancers declared by modules (`AppGuard<G>` and friends) with their declaring
    /// module, imported modules first.
    pub(crate) app_enhancers: Vec<(usize, EnhancerRef)>,
    /// Every controller route, also injectable as `Arc<Routes>` from the core module.
    pub(crate) routes: Arc<Routes>,
    /// Modules identified by their type.
    by_type: HashMap<TypeId, usize>,
    /// Modules identified by their type and key.
    keyed: Vec<(usize, ModuleKey)>,
    /// Modules that carry data without declaring an identity, with the importer of each import
    /// of their type (`None` for the root).
    undeclared: Vec<(usize, Vec<Option<usize>>)>,
    overrides: Vec<Override>,
    problems: Vec<Problem>,
}

struct CoreModule;

impl Graph {
    pub(crate) fn build(
        root: &dyn Module,
        shutdown: Shutdown,
        prefix: &str,
        global: &GlobalEnhancers,
        overrides: Vec<(Key, ProviderDef)>,
    ) -> Result<Self, BootError> {
        let mut graph = Graph {
            modules: Vec::new(),
            nodes: Vec::new(),
            root: 0,
            order: Vec::new(),
            effective: Vec::new(),
            report: ScopeReport::default(),
            app_enhancers: Vec::new(),
            routes: Arc::default(),
            by_type: HashMap::new(),
            keyed: Vec::new(),
            undeclared: Vec::new(),
            overrides: Vec::new(),
            problems: Vec::new(),
        };
        // The last override registered for a key wins; the ones it replaces are not reported.
        for (key, def) in overrides {
            match graph.overrides.iter_mut().find(|o| o.key == key) {
                Some(existing) => existing.def = def,
                None => graph.overrides.push(Override {
                    key,
                    def,
                    used: false,
                }),
            }
        }

        graph.add_core(shutdown);
        graph.root = graph
            .collect(root, &mut Vec::new())
            .expect("the root module cannot be part of an import cycle");
        graph.label_instances();
        graph.report_undeclared_identities();
        graph.check_global_enhancers();
        graph.compute_exports();
        graph.add_route_table(prefix, global);

        let root = graph.root;
        for enhancer in global
            .guards
            .iter()
            .chain(&global.interceptors)
            .chain(&global.filters)
            .chain(&global.middlewares)
        {
            graph.ensure_slot(root, enhancer);
        }
        for (module, enhancer) in graph.app_enhancers.clone() {
            graph.ensure_slot(module, &enhancer);
        }
        for module in 0..graph.modules.len() {
            let enhancers: Vec<EnhancerRef> = graph.modules[module]
                .controllers
                .iter()
                .flat_map(|(_, controller)| controller.enhancers().cloned())
                .chain(graph.modules[module].middlewares.iter().cloned())
                .collect();
            for enhancer in &enhancers {
                graph.ensure_slot(module, enhancer);
            }
        }

        graph.resolve_edges();
        for unused in graph.overrides.iter().filter(|o| !o.used) {
            graph.problems.push(Problem::UnusedOverride {
                provider: unused.key.short_name(),
            });
        }
        graph.bail()?;

        graph.order = graph.topological_order();
        graph.bail()?;

        graph.compute_scopes();
        graph.bail()?;
        Ok(graph)
    }

    fn bail(&mut self) -> Result<(), BootError> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(BootError::new(std::mem::take(&mut self.problems)))
        }
    }

    fn add_core(&mut self, shutdown: Shutdown) {
        let mut core = ModuleNode::new(TypeId::of::<CoreModule>(), "hornero".into(), true, None);
        for def in [ProviderDef::value(shutdown), ProviderDef::request_context()] {
            let key = def.key;
            let node = self.push_node(def, 0, NodeKind::Builtin, key.short_name());
            core.local.insert(key, node);
            core.exported.insert(key, node);
        }
        self.modules.push(core);
    }

    /// Lists every controller route, with the guards that run for it in the order `assemble`
    /// runs them, and provides the table from the core module as `Arc<Routes>`.
    fn add_route_table(&mut self, prefix: &str, global: &GlobalEnhancers) {
        let global_guards: Vec<EnhancerInfo> = global
            .guards
            .iter()
            .chain(
                self.app_enhancers
                    .iter()
                    .filter(|(_, enhancer)| enhancer.kind == EnhancerKind::Guard)
                    .map(|(_, enhancer)| enhancer),
            )
            .map(EnhancerRef::info)
            .collect();

        let mut routes = Vec::new();
        for module in &self.modules {
            for (_, controller) in &module.controllers {
                let controller_guards: Vec<EnhancerInfo> =
                    controller.guards.iter().map(EnhancerRef::info).collect();
                for route in &controller.routes {
                    let mut meta = controller.meta.clone();
                    meta.extend(&route.meta);
                    routes.push(RouteInfo {
                        method: route.method.clone(),
                        path: join_paths(&[prefix, &controller.prefix, &route.path]),
                        controller: controller.name.clone(),
                        handler: route.name,
                        module: module.name.clone(),
                        meta,
                        guards: global_guards
                            .iter()
                            .cloned()
                            .chain(controller_guards.iter().cloned())
                            .chain(route.guards.iter().map(EnhancerRef::info))
                            .collect(),
                    });
                }
            }
        }

        let prefix = match join_paths(&[prefix]) {
            root if root == "/" => String::new(),
            prefix => prefix,
        };
        let routes = Arc::new(Routes { prefix, routes });
        let def = ProviderDef::from_arc(routes.clone());
        let key = def.key;
        let node = self.push_node(def, 0, NodeKind::Builtin, key.short_name());
        self.modules[0].local.insert(key, node);
        self.modules[0].exported.insert(key, node);
        self.routes = routes;
    }

    fn collect(&mut self, module: &dyn Module, stack: &mut Vec<usize>) -> Option<usize> {
        let type_id = (module as &dyn std::any::Any).type_id();
        let importer = stack.last().copied();
        let identity = match module.identity() {
            // A zero-sized type has a single value, so merging its imports loses nothing.
            Identity::Auto if size_of_val(module) == 0 => Identity::Type,
            identity => identity,
        };

        if let Identity::Auto = identity
            && let Some(i) = self
                .undeclared
                .iter()
                .position(|&(m, _)| self.modules[m].type_id == type_id)
        {
            // Reported once per type after collection. Meanwhile the import stands for the first
            // one, unless that one is importing it, which would close an import cycle.
            self.undeclared[i].1.push(importer);
            let first = self.undeclared[i].0;
            return (!stack.contains(&first)).then_some(first);
        }

        // A cycle repeats an identity on the stack. Instances never repeat theirs, so for them
        // any module of the same type on the stack counts: importing it would recurse forever.
        let repeats = |graph: &Self, m: usize| {
            graph.modules[m].type_id == type_id
                && match &identity {
                    Identity::Key(key) => {
                        graph.keyed.iter().any(|(k, other)| *k == m && other == key)
                    }
                    _ => true,
                }
        };
        if let Some(position) = stack.iter().position(|&m| repeats(self, m)) {
            let mut path: Vec<String> = stack[position..]
                .iter()
                .map(|&m| self.modules[m].name.clone())
                .collect();
            path.push(module.name());
            self.problems.push(Problem::CircularImport { path });
            return None;
        }

        let existing = match &identity {
            Identity::Type => self.by_type.get(&type_id).copied(),
            Identity::Key(key) => self
                .keyed
                .iter()
                .find(|(m, other)| self.modules[*m].type_id == type_id && other == key)
                .map(|&(m, _)| m),
            Identity::Auto | Identity::Instance => None,
        };
        if existing.is_some() {
            return existing;
        }

        let mut def = ModuleDef::default();
        module.define(&mut def);
        let idx = self.modules.len();
        self.modules.push(ModuleNode::new(
            type_id,
            module.name(),
            def.global,
            importer,
        ));
        match identity {
            Identity::Type => {
                self.by_type.insert(type_id, idx);
            }
            Identity::Key(key) => self.keyed.push((idx, key)),
            Identity::Auto => self.undeclared.push((idx, vec![importer])),
            Identity::Instance => {}
        }

        stack.push(idx);
        for import in std::mem::take(&mut def.imports) {
            if let Some(imported) = self.collect(import.as_ref(), stack)
                && !self.modules[idx].imports.contains(&imported)
            {
                self.modules[idx].imports.push(imported);
            }
        }
        stack.pop();

        for provider in def.providers {
            self.add_provider(idx, provider);
        }
        for controller in def.controllers {
            let node = self.push_node(
                controller.def.clone(),
                idx,
                NodeKind::Controller,
                controller.name.clone(),
            );
            self.modules[idx].controllers.push((node, controller));
        }
        for mount in def.mounts {
            let label = format!("mount {}", mount.path);
            let node = self.push_node(mount.def, idx, NodeKind::Mount, label);
            self.modules[idx]
                .mounts
                .push((mount.path, node, mount.router));
        }
        // Imports were collected above, so their global enhancers come first.
        for enhancer in def.app_enhancers {
            let duplicate = self
                .app_enhancers
                .iter()
                .any(|(module, e)| *module == idx && e.slot_key == enhancer.slot_key);
            if duplicate {
                self.problems.push(Problem::DuplicateProvider {
                    provider: enhancer.token_name(),
                    module: self.modules[idx].name.clone(),
                });
                continue;
            }
            self.app_enhancers.push((idx, enhancer));
        }
        let module = &mut self.modules[idx];
        module.exports = def.exports;
        module.export_modules = def.export_modules;
        module.middlewares = def.middlewares;
        module.layers = def.layers;
        Some(idx)
    }

    /// Instances of one module type share a name. Numbers them in collection order and names the
    /// module that imported each, so diagnostics, the scope report and the route table tell them
    /// apart.
    fn label_instances(&mut self) {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for module in &self.modules {
            *counts.entry(&module.base_name).or_default() += 1;
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let labels: Vec<Option<String>> = self
            .modules
            .iter()
            .map(|module| {
                if counts[module.base_name.as_str()] < 2 {
                    return None;
                }
                let n = seen.entry(&module.base_name).or_default();
                *n += 1;
                Some(match module.importer {
                    Some(importer) => format!(
                        "{}#{n}, imported by {}",
                        module.base_name, self.modules[importer].base_name
                    ),
                    None => format!("{}#{n}", module.base_name),
                })
            })
            .collect();
        for (module, label) in self.modules.iter_mut().zip(labels) {
            if let Some(label) = label {
                module.name = label;
            }
        }
    }

    fn report_undeclared_identities(&mut self) {
        for (module, importers) in std::mem::take(&mut self.undeclared) {
            if importers.len() < 2 {
                continue;
            }
            self.problems.push(Problem::UndeclaredModuleIdentity {
                module: self.modules[module].name.clone(),
                imported_by: importers
                    .iter()
                    .map(|importer| importer.map(|m| self.modules[m].name.clone()))
                    .collect(),
            });
        }
    }

    /// A global enhancer runs once per declaring module on every route. Distinct module types
    /// declaring the same one each run it, but instances of one type would multiply it at their
    /// import sites, out of sight of the declaration, so that is an error.
    fn check_global_enhancers(&mut self) {
        let mut declared: Vec<(TypeId, &EnhancerRef, Vec<usize>)> = Vec::new();
        for (module, enhancer) in &self.app_enhancers {
            let type_id = self.modules[*module].type_id;
            match declared
                .iter_mut()
                .find(|(t, e, _)| *t == type_id && e.slot_key == enhancer.slot_key)
            {
                Some((_, _, modules)) => modules.push(*module),
                None => declared.push((type_id, enhancer, vec![*module])),
            }
        }
        let problems: Vec<Problem> = declared
            .into_iter()
            .filter(|(_, _, modules)| modules.len() > 1)
            .map(|(_, enhancer, modules)| Problem::DuplicateGlobalEnhancer {
                enhancer: enhancer.token_name(),
                module: self.modules[modules[0]].base_name.clone(),
                instances: modules
                    .iter()
                    .map(|&m| self.modules[m].name.clone())
                    .collect(),
            })
            .collect();
        self.problems.extend(problems);
    }

    fn add_provider(&mut self, module: usize, def: ProviderDef) {
        let key = def.key;
        if self.modules[module].local.contains_key(&key) {
            self.problems.push(Problem::DuplicateProvider {
                provider: key.short_name(),
                module: self.modules[module].name.clone(),
            });
            return;
        }
        let node = self.push_node(def, module, NodeKind::Provider, key.short_name());
        self.modules[module].local.insert(key, node);
    }

    fn push_node(
        &mut self,
        mut def: ProviderDef,
        module: usize,
        kind: NodeKind,
        label: String,
    ) -> usize {
        if matches!(kind, NodeKind::Provider | NodeKind::Slot(_))
            && let Some(o) = self.overrides.iter_mut().find(|o| o.key == def.key)
        {
            o.used = true;
            def = o.def.clone();
        }
        let invalid = def.invalid.clone();
        self.nodes.push(Node {
            def,
            module,
            kind,
            label,
            deps: Vec::new(),
            absent: Vec::new(),
            preset: false,
        });
        let node = self.nodes.len() - 1;
        if let Some(reason) = invalid {
            self.problems.push(Problem::InvalidProvider {
                provider: self.describe(node),
                reason,
            });
        }
        node
    }

    fn compute_exports(&mut self) {
        let mut state = vec![0u8; self.modules.len()];
        state[0] = 2; // the core module's exports are fixed
        for module in 0..self.modules.len() {
            self.exports_of(module, &mut state);
        }
    }

    fn exports_of(&mut self, module: usize, state: &mut [u8]) {
        if state[module] != 0 {
            return;
        }
        state[module] = 1;
        let imports = self.modules[module].imports.clone();
        for &import in &imports {
            self.exports_of(import, state);
        }

        let mut exported = HashMap::new();
        let explicit = self.modules[module].exports.clone();
        for &key in &explicit {
            if let Some(&node) = self.modules[module].local.get(&key) {
                exported.insert(key, node);
                continue;
            }
            let candidates = distinct(
                imports
                    .iter()
                    .filter_map(|&import| self.modules[import].exported.get(&key).copied()),
            );
            if candidates.is_empty() {
                self.problems.push(Problem::InvalidExport {
                    module: self.modules[module].name.clone(),
                    export: key.short_name(),
                });
                continue;
            }
            self.export_resolved(module, key, &candidates, &mut exported);
        }

        // Every import of a re-exported module type counts. Keys exported explicitly were settled
        // above: a local provider shadows the re-exported one, as it does inside the module, and
        // an ambiguity is reported once.
        let mut reexported: HashMap<Key, Vec<usize>> = HashMap::new();
        for (type_id, name) in self.modules[module].export_modules.clone() {
            let matching: Vec<usize> = imports
                .iter()
                .copied()
                .filter(|&import| self.modules[import].type_id == type_id)
                .collect();
            if matching.is_empty() {
                self.problems.push(Problem::InvalidModuleExport {
                    module: self.modules[module].name.clone(),
                    export: short_type_name(name),
                });
            }
            for import in matching {
                for (&key, &node) in &self.modules[import].exported {
                    if !explicit.contains(&key) {
                        reexported.entry(key).or_default().push(node);
                    }
                }
            }
        }
        // `exported` maps iterate in random order; sorting by the full name keeps the problems'
        // order stable even when two keys share a short name.
        let mut reexported: Vec<(Key, Vec<usize>)> = reexported.into_iter().collect();
        reexported.sort_by_cached_key(|(key, _)| format!("{key:?}"));
        for (key, nodes) in reexported {
            let candidates = distinct(nodes.into_iter());
            self.export_resolved(module, key, &candidates, &mut exported);
        }
        self.modules[module].exported = exported;
        state[module] = 2;
    }

    /// Exports `key` from `module` when a single node provides it, and reports the ambiguity
    /// otherwise.
    fn export_resolved(
        &mut self,
        module: usize,
        key: Key,
        candidates: &[usize],
        exported: &mut HashMap<Key, usize>,
    ) {
        match candidates {
            [node] => {
                exported.insert(key, *node);
            }
            _ => {
                let consumer = format!("re-export in {}", self.modules[module].name);
                let problem = self.ambiguous(consumer, key, candidates);
                self.problems.push(problem);
            }
        }
    }

    /// The problem of `consumer` seeing `key` from several `candidates`.
    fn ambiguous(&self, consumer: String, key: Key, candidates: &[usize]) -> Problem {
        let dependency = key.short_name();
        let modules = distinct(candidates.iter().map(|&n| self.nodes[n].module));
        let first = &self.modules[modules[0]];
        let hint = if modules.len() > 1
            && modules
                .iter()
                .all(|&m| self.modules[m].type_id == first.type_id)
        {
            format!(
                "the sources are separate instances of {}: import it once and share that instance, or give each instance's providers a type of its own, such as a generic tag.",
                first.base_name
            )
        } else {
            format!(
                "provide `{dependency}` locally, or import only one of the modules that export it (a global module counts as imported everywhere)."
            )
        };
        Problem::AmbiguousDependency {
            consumer,
            dependency,
            candidates: candidates.iter().map(|&n| self.describe(n)).collect(),
            hint,
        }
    }

    /// Resolves `key` as seen from `module`: local providers, then imported exports, then globals.
    pub(crate) fn lookup(&self, module: usize, key: Key) -> Lookup {
        let current = &self.modules[module];
        if let Some(&node) = current.local.get(&key) {
            return Lookup::Found(node);
        }

        let from_imports = distinct(
            current
                .imports
                .iter()
                .filter_map(|&import| self.modules[import].exported.get(&key).copied()),
        );
        match from_imports.len() {
            0 => {}
            1 => return Lookup::Found(from_imports[0]),
            _ => return Lookup::Ambiguous(from_imports),
        }

        let from_globals = distinct(
            self.modules
                .iter()
                .enumerate()
                .filter(|(idx, m)| m.global && *idx != module)
                .filter_map(|(_, m)| m.exported.get(&key).copied()),
        );
        match from_globals.len() {
            0 => Lookup::Missing,
            1 => Lookup::Found(from_globals[0]),
            _ => Lookup::Ambiguous(from_globals),
        }
    }

    fn ensure_slot(&mut self, module: usize, enhancer: &EnhancerRef) -> usize {
        if let Some(&slot) = self.modules[module].slots.get(&enhancer.slot_key) {
            return slot;
        }
        let label = format!("{} {}", enhancer.kind.label(), enhancer.name);
        let overridden = self.overrides.iter().any(|o| o.key == enhancer.slot_key);
        let slot = if overridden {
            // The replacement has no dependencies, so the original enhancer is never needed.
            self.push_node(
                (enhancer.make_slot)(),
                module,
                NodeKind::Slot(enhancer.kind),
                label,
            )
        } else {
            let target = self.enhancer_target(module, enhancer, &label);
            let slot = self.push_node(
                (enhancer.make_slot)(),
                module,
                NodeKind::Slot(enhancer.kind),
                label,
            );
            // Without a target the ambiguity is already reported; the slot has nothing to wait for.
            self.nodes[slot].deps = target.into_iter().collect();
            slot
        };
        self.nodes[slot].preset = true;
        self.modules[module].slots.insert(enhancer.slot_key, slot);
        slot
    }

    /// Finds the enhancer instance: a visible provider if there is one, otherwise an implicit
    /// provider private to `module` (Nest instantiates `@UseGuards(X)` the same way). Several
    /// visible providers are ambiguous, as for any other dependency: `None` after reporting it.
    fn enhancer_target(
        &mut self,
        module: usize,
        enhancer: &EnhancerRef,
        label: &str,
    ) -> Option<usize> {
        let key = enhancer.target_key;
        match self.lookup(module, key) {
            Lookup::Found(node) => Some(node),
            Lookup::Ambiguous(candidates) => {
                let consumer = format!("{label} (in {})", self.modules[module].name);
                let problem = self.ambiguous(consumer, key, &candidates);
                self.problems.push(problem);
                None
            }
            Lookup::Missing => {
                if let Some(&node) = self.modules[module].implicit.get(&key) {
                    return Some(node);
                }
                let node = self.push_node(
                    (enhancer.make_target)(),
                    module,
                    NodeKind::Provider,
                    enhancer.name.clone(),
                );
                self.modules[module].implicit.insert(key, node);
                Some(node)
            }
        }
    }

    fn resolve_edges(&mut self) {
        for idx in 0..self.nodes.len() {
            if self.nodes[idx].preset {
                continue;
            }
            let module = self.nodes[idx].module;
            let mut resolved = Vec::with_capacity(self.nodes[idx].def.deps.len());
            let mut absent = Vec::new();
            for key in self.nodes[idx].def.deps.clone() {
                match self.lookup(module, key) {
                    // A consumer declaring the same type twice receives one instance.
                    Lookup::Found(node) if resolved.contains(&node) => {}
                    Lookup::Found(node) => resolved.push(node),
                    Lookup::Missing if key.is_optional() => absent.push(key),
                    Lookup::Missing => self.problems.push(Problem::MissingDependency {
                        consumer: self.describe(idx),
                        dependency: key.short_name(),
                        hint: self.missing_hint(module, key),
                    }),
                    Lookup::Ambiguous(candidates) => {
                        let problem = self.ambiguous(self.describe(idx), key, &candidates);
                        self.problems.push(problem);
                    }
                }
            }
            self.nodes[idx].deps = resolved;
            self.nodes[idx].absent = absent;
        }
    }

    fn missing_hint(&self, module: usize, key: Key) -> String {
        let name = &self.modules[module].name;
        let dependency = key.short_name();
        let others = || {
            self.modules
                .iter()
                .enumerate()
                .filter(move |(idx, _)| *idx != module)
                .map(|(_, m)| m)
        };

        let exporters: Vec<&str> = others()
            .filter(|m| m.exported.contains_key(&key))
            .map(|m| m.name.as_str())
            .collect();
        if !exporters.is_empty() {
            return format!(
                "`{dependency}` is exported by {}; add it to the imports of {name}.",
                exporters.join(", ")
            );
        }

        if let Some(provider) = others().find(|m| m.local.contains_key(&key)) {
            let provider = &provider.name;
            return format!(
                "`{dependency}` is provided by {provider} but not exported; add it to the exports of {provider} and import {provider} in {name}."
            );
        }

        // The type is provided, only under another qualifier (or none).
        let variants: Vec<String> = self
            .modules
            .iter()
            .flat_map(|m| {
                let mut keys: Vec<String> = m
                    .local
                    .keys()
                    .filter(|k| k.type_id() == key.type_id())
                    .map(|k| format!("`{k}` (in {})", m.name))
                    .collect();
                keys.sort();
                keys
            })
            .collect();
        if !variants.is_empty() {
            return format!(
                "no module provides `{dependency}`, but the same type is provided as {}; check the qualifier (`#[inject(qualifier = Q)]` on the consumer, `.qualified::<Q>()` on the provider).",
                variants.join(", ")
            );
        }

        format!(
            "no module provides `{dependency}`; register it in the providers of {name} or of a module {name} imports."
        )
    }

    /// `"TodoService (in TodoModule)"`.
    pub(crate) fn describe(&self, node: usize) -> String {
        let node = &self.nodes[node];
        format!("{} (in {})", node.label, self.modules[node.module].name)
    }

    fn topological_order(&mut self) -> Vec<usize> {
        let mut color = vec![0u8; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack = Vec::new();
        for node in 0..self.nodes.len() {
            self.visit(node, &mut color, &mut stack, &mut order);
        }
        order
    }

    fn visit(
        &mut self,
        node: usize,
        color: &mut [u8],
        stack: &mut Vec<usize>,
        order: &mut Vec<usize>,
    ) {
        match color[node] {
            2 => return,
            1 => {
                let start = stack.iter().position(|&n| n == node).unwrap_or(0);
                let mut path: Vec<String> =
                    stack[start..].iter().map(|&n| self.describe(n)).collect();
                path.push(self.describe(node));
                self.problems.push(Problem::CircularDependency { path });
                return;
            }
            _ => {}
        }
        color[node] = 1;
        stack.push(node);
        for dep in self.nodes[node].deps.clone() {
            self.visit(dep, color, stack, order);
        }
        stack.pop();
        color[node] = 2;
        order.push(node);
    }

    fn compute_scopes(&mut self) {
        let count = self.nodes.len();
        let mut effective = vec![Scope::Singleton; count];
        let mut cause: Vec<Option<usize>> = vec![None; count];
        for &node in &self.order {
            let declared = self.nodes[node].def.scope;
            effective[node] = declared;
            if declared != Scope::Request
                && let Some(&dep) = self.nodes[node]
                    .deps
                    .iter()
                    .find(|&&dep| effective[dep] == Scope::Request)
            {
                effective[node] = Scope::Request;
                cause[node] = Some(dep);
            }
        }

        let chain = |start: usize| -> Vec<String> {
            let mut out = Vec::new();
            let mut current = Some(start);
            while let Some(node) = current {
                // Slots mirror their enhancer; listing both would repeat the name.
                if !matches!(self.nodes[node].kind, NodeKind::Slot(_)) {
                    out.push(self.nodes[node].label.clone());
                }
                current = cause[node];
            }
            if let Some(last) = out.last_mut() {
                last.push_str(" [request]");
            }
            out
        };

        let mut problems = Vec::new();
        let mut report = ScopeReport::default();
        for &node in &self.order {
            let n = &self.nodes[node];
            let bubbled = cause[node].is_some();
            let item = self.describe(node);

            if bubbled && n.def.pinned {
                problems.push(Problem::ScopeViolation {
                    item: item.clone(),
                    reason: format!(
                        "is pinned to {} scope but depends on a request-scoped provider",
                        scope_name(n.def.scope)
                    ),
                    chain: chain(node),
                });
            }
            if n.def.has_hooks && effective[node] != Scope::Singleton {
                problems.push(Problem::ScopeViolation {
                    item: item.clone(),
                    reason: format!(
                        "has lifecycle hooks but is {}; hooks only run on singletons",
                        if effective[node] == Scope::Request {
                            "request-scoped"
                        } else {
                            "transient"
                        }
                    ),
                    chain: if bubbled { chain(node) } else { Vec::new() },
                });
            }
            if effective[node] == Scope::Request {
                let forbidden = match n.kind {
                    NodeKind::Slot(EnhancerKind::Middleware) => Some("cannot be request-scoped"),
                    NodeKind::Mount => Some("cannot be request-scoped"),
                    _ => None,
                };
                if let Some(reason) = forbidden {
                    problems.push(Problem::ScopeViolation {
                        item: item.clone(),
                        reason: reason.into(),
                        chain: chain(node),
                    });
                }
                if matches!(n.kind, NodeKind::Provider | NodeKind::Controller) {
                    report.entries.push(ScopeEntry {
                        item: n.label.clone(),
                        module: self.modules[n.module].name.clone(),
                        declared: !bubbled,
                        chain: if bubbled { chain(node) } else { Vec::new() },
                    });
                }
            }
        }

        self.problems.extend(problems);
        self.effective = effective;
        self.report = report;
    }

    pub(crate) fn runtime(&self) -> Runtime {
        let count = self.nodes.len();
        let mut needs_context = vec![false; count];
        for &node in &self.order {
            let n = &self.nodes[node];
            needs_context[node] = n.def.builtin == Builtin::RequestContext
                || (self.effective[node] != Scope::Singleton
                    && n.deps.iter().any(|&dep| needs_context[dep]));
        }

        let mut slot_count = 0;
        let nodes = (0..count)
            .map(|idx| {
                let n = &self.nodes[idx];
                let slot = (self.effective[idx] == Scope::Request
                    && n.def.builtin == Builtin::None)
                    .then(|| {
                        slot_count += 1;
                        slot_count - 1
                    });
                RtNode {
                    key: n.def.key,
                    label: Arc::from(self.describe(idx)),
                    scope: self.effective[idx],
                    deps: n.deps.clone(),
                    absent: n.absent.clone(),
                    factory: n.def.factory.clone(),
                    builtin: n.def.builtin,
                    slot,
                    needs_context: needs_context[idx],
                }
            })
            .collect();

        Runtime {
            nodes,
            singletons: (0..count).map(|_| OnceLock::new()).collect(),
            slot_count,
        }
    }

    /// Singleton providers reachable by key from outside, for `App::get` in tests.
    pub(crate) fn singleton_index(&self) -> HashMap<Key, usize> {
        let mut candidates: HashMap<Key, Vec<usize>> = HashMap::new();
        for (idx, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind, NodeKind::Provider | NodeKind::Builtin)
                && self.effective[idx] == Scope::Singleton
            {
                candidates.entry(node.def.key).or_default().push(idx);
            }
        }
        candidates
            .into_iter()
            .filter_map(|(key, nodes)| match self.lookup(self.root, key) {
                Lookup::Found(node) if nodes.contains(&node) => Some((key, node)),
                _ if nodes.len() == 1 => Some((key, nodes[0])),
                _ => None,
            })
            .collect()
    }
}

fn distinct(iter: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut out = Vec::new();
    for value in iter {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Singleton => "singleton",
        Scope::Request => "request",
        Scope::Transient => "transient",
    }
}

/// Every component that ends up request-scoped, and why.
///
/// Logged at bootstrap and available through [`App::scope_report`](crate::App::scope_report) or
/// [`Hornero::validate`](crate::Hornero::validate), so request-scope bubbling never goes unnoticed.
#[derive(Clone, Debug, Default)]
pub struct ScopeReport {
    /// One entry per request-scoped component, in dependency order.
    pub entries: Vec<ScopeEntry>,
}

/// A request-scoped component in a [`ScopeReport`].
#[derive(Clone, Debug)]
pub struct ScopeEntry {
    /// Short type name of the component.
    pub item: String,
    /// The module that registers it.
    pub module: String,
    /// `true` when declared request-scoped, `false` when pulled in by a dependency.
    pub declared: bool,
    /// For bubbled entries: the dependency chain ending at a declared request-scoped provider.
    pub chain: Vec<String>,
}

impl ScopeReport {
    /// Whether nothing is request-scoped.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry for the component with this short type name.
    pub fn find(&self, item: &str) -> Option<&ScopeEntry> {
        self.entries.iter().find(|entry| entry.item == item)
    }
}

impl fmt::Display for ScopeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for entry in &self.entries {
            if entry.declared {
                writeln!(
                    f,
                    "  {} (in {}): declared request-scoped",
                    entry.item, entry.module
                )?;
            } else {
                writeln!(
                    f,
                    "  {} (in {}): request-scoped via {}",
                    entry.item,
                    entry.module,
                    entry.chain.join(" -> ")
                )?;
            }
        }
        Ok(())
    }
}
