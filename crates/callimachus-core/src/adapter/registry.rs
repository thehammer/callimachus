use std::collections::HashMap;
use std::sync::Arc;

use super::contract::SourceAdapter;

/// Registry that maps adapter kind strings to adapter implementations.
///
/// The indexing pipeline looks up the correct adapter for a corpus via this
/// registry.
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn SourceAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    /// Register an adapter under its `kind()` key.
    pub fn register(&mut self, adapter: Arc<dyn SourceAdapter>) {
        self.adapters.insert(adapter.kind().to_string(), adapter);
    }

    /// Look up an adapter by kind string.
    pub fn get(&self, kind: &str) -> Option<Arc<dyn SourceAdapter>> {
        self.adapters.get(kind).cloned()
    }

    /// List registered kind strings.
    pub fn list(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.adapters.keys().map(String::as_str).collect();
        keys.sort();
        keys
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}
