use std::sync::{Arc, OnceLock};

use super::injectable::Scope;
use super::key::{Instance, Key};
use super::provider::{Built, Factory};
use super::resolver::Resolver;
use crate::util::{BoxError, BoxFuture};

/// A provider node after validation: dependency edges point at concrete nodes.
pub(crate) struct RtNode {
    pub(crate) key: Key,
    pub(crate) label: Arc<str>,
    pub(crate) scope: Scope,
    pub(crate) deps: Vec<usize>,
    /// Declared optional dependencies that nothing provides; the resolver answers them with `None`.
    pub(crate) absent: Vec<Key>,
    pub(crate) factory: Factory,
}

pub(crate) struct Runtime {
    pub(crate) nodes: Vec<RtNode>,
    pub(crate) singletons: Vec<OnceLock<Instance>>,
}

#[derive(Debug)]
pub(crate) struct ResolveError {
    pub(crate) item: Arc<str>,
    pub(crate) source: BoxError,
}

impl Runtime {
    /// Builds a fresh instance of `idx`, resolving its dependencies first.
    pub(crate) fn instantiate(&self, idx: usize) -> BoxFuture<'_, Result<Built, ResolveError>> {
        Box::pin(async move {
            let node = &self.nodes[idx];
            let mut entries = Vec::with_capacity(node.deps.len());
            for &dep in &node.deps {
                let instance = self.get(dep).await?;
                entries.push((self.nodes[dep].key, instance));
            }
            let resolver = Resolver::new(node.label.clone(), entries, node.absent.clone());
            (node.factory)(resolver)
                .await
                .map_err(|source| ResolveError {
                    item: node.label.clone(),
                    source,
                })
        })
    }

    /// Returns the instance a consumer should receive for `idx`, honouring its scope.
    fn get(&self, idx: usize) -> BoxFuture<'_, Result<Instance, ResolveError>> {
        Box::pin(async move {
            let node = &self.nodes[idx];
            match node.scope {
                Scope::Singleton => {
                    self.singletons[idx]
                        .get()
                        .cloned()
                        .ok_or_else(|| ResolveError {
                            item: node.label.clone(),
                            source: "singleton was not constructed during bootstrap".into(),
                        })
                }
                Scope::Transient => Ok(self.instantiate(idx).await?.instance),
            }
        })
    }
}
