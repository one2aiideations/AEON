//! Capability-checked tool bus.
//!
//! A tool declares the capability bits it needs; a caller must hold all of
//! them to invoke it.

use core::fmt;

/// Tool handler: bytes in, bytes out.
pub type Handler = Box<dyn Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync>;

/// Tool bus errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Bus is full.
    Full,
    /// Tool id already registered.
    Duplicate,
    /// No such tool.
    Unknown,
    /// Caller lacks required capabilities.
    Denied,
    /// The tool reported a failure.
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("tool bus full"),
            Self::Duplicate => f.write_str("tool already registered"),
            Self::Unknown => f.write_str("unknown tool"),
            Self::Denied => f.write_str("missing capability"),
            Self::Failed(e) => write!(f, "tool failed: {e}"),
        }
    }
}

impl std::error::Error for ToolError {}

struct Entry {
    id: u32,
    required: u32,
    handler: Handler,
}

/// Registered tools.
pub struct ToolBus {
    entries: Vec<Entry>,
    cap: usize,
}

impl fmt::Debug for ToolBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolBus")
            .field(
                "tools",
                &self.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            )
            .field("cap", &self.cap)
            .finish()
    }
}

impl ToolBus {
    /// Bus holding at most `cap` tools.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            entries: Vec::new(),
            cap,
        }
    }

    /// Register tool `id` requiring capability bits `required`.
    pub fn register(&mut self, id: u32, required: u32, handler: Handler) -> Result<(), ToolError> {
        if self.entries.len() >= self.cap {
            return Err(ToolError::Full);
        }
        if self.entries.iter().any(|e| e.id == id) {
            return Err(ToolError::Duplicate);
        }
        self.entries.push(Entry {
            id,
            required,
            handler,
        });
        Ok(())
    }

    /// Call tool `id` holding capability bits `caps`.
    pub fn invoke(&self, id: u32, caps: u32, input: &[u8]) -> Result<Vec<u8>, ToolError> {
        let e = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or(ToolError::Unknown)?;
        if e.required & caps != e.required {
            return Err(ToolError::Denied);
        }
        (e.handler)(input).map_err(ToolError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_checks() {
        let mut bus = ToolBus::new(2);
        bus.register(
            1,
            0b11,
            Box::new(|i: &[u8]| Ok(i.iter().rev().copied().collect())),
        )
        .unwrap();
        bus.register(2, 0, Box::new(|_: &[u8]| Err("boom".into())))
            .unwrap();
        assert_eq!(
            bus.register(3, 0, Box::new(|_: &[u8]| Ok(vec![]))),
            Err(ToolError::Full)
        );
        assert_eq!(bus.invoke(1, 0b111, b"ab").unwrap(), b"ba");
        assert_eq!(bus.invoke(1, 0b01, b"ab"), Err(ToolError::Denied));
        assert_eq!(bus.invoke(2, 0, b""), Err(ToolError::Failed("boom".into())));
        assert_eq!(bus.invoke(9, 0, b""), Err(ToolError::Unknown));
    }
}
