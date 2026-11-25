use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExecutionBoundary {
    pub max_cost: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub network_allowed: bool,
    pub physical_control_allowed: bool,
    pub financial_control_allowed: bool,
    pub geographic_scope: Option<String>,
}

impl ExecutionBoundary {
    pub fn permits_network(&self) -> bool {
        self.network_allowed
    }

    pub fn permits_physical_control(&self) -> bool {
        self.physical_control_allowed
    }

    pub fn permits_financial_control(&self) -> bool {
        self.financial_control_allowed
    }
}
