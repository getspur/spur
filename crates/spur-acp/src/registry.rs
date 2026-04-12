use crate::config::AgentConfig;
use crate::types::{AgentHealth, AgentRole, TransportKind};
use std::collections::HashMap;

/// Registry of all known agents and their configurations.
pub struct AgentRegistry {
    agents: HashMap<String, AgentEntry>,
}

struct AgentEntry {
    config: AgentConfig,
    health: AgentHealth,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Load agents from a list of configs (parsed from agents.toml).
    pub fn load(configs: Vec<AgentConfig>) -> Self {
        let mut registry = Self::new();
        for config in configs {
            registry.register(config);
        }
        registry
    }

    /// Register a new agent.
    pub fn register(&mut self, config: AgentConfig) {
        let name = config.name.clone();
        self.agents.insert(
            name,
            AgentEntry {
                config,
                health: AgentHealth::Unknown,
            },
        );
    }

    /// Remove an agent by name.
    pub fn remove(&mut self, name: &str) -> bool {
        self.agents.remove(name).is_some()
    }

    /// Get agent config by name.
    pub fn get(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.get(name).map(|e| &e.config)
    }

    /// List all registered agents.
    pub fn list(&self) -> Vec<&AgentConfig> {
        self.agents.values().map(|e| &e.config).collect()
    }

    /// List agents that can serve as brain.
    pub fn brain_capable(&self) -> Vec<&AgentConfig> {
        self.agents
            .values()
            .filter(|e| matches!(e.config.role, AgentRole::Brain | AgentRole::Both))
            .map(|e| &e.config)
            .collect()
    }

    /// List agents that can serve as workers.
    pub fn worker_capable(&self) -> Vec<&AgentConfig> {
        self.agents
            .values()
            .filter(|e| matches!(e.config.role, AgentRole::Worker | AgentRole::Both))
            .map(|e| &e.config)
            .collect()
    }

    /// Get current health of an agent.
    pub fn health(&self, name: &str) -> Option<&AgentHealth> {
        self.agents.get(name).map(|e| &e.health)
    }

    /// Update health status of an agent.
    pub fn set_health(&mut self, name: &str, health: AgentHealth) {
        if let Some(entry) = self.agents.get_mut(name) {
            entry.health = health;
        }
    }

    /// Get the transport kind for an agent.
    pub fn transport_kind(&self, name: &str) -> Option<TransportKind> {
        self.agents.get(name).map(|e| e.config.transport)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
