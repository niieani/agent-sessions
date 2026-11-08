//! Source-native grouping facts, independent of recorded filesystem workspaces.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeProject {
    pub source: String,
    pub native_id: String,
    pub name: Option<String>,
}

impl NativeProject {
    /// Source-qualified Name Table key, stable across native display-name changes.
    pub fn key(&self) -> String {
        format!("{}:{}", self.source, self.native_id)
    }
}
