pub mod delegation;
pub mod signals;

pub fn brain_tool_registry(
    delegation_deps: delegation::DelegationMcpDeps,
    signal_deps: signals::SignalMcpDeps,
) -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    let builder = spur_mcp::ToolRegistry::builder()
        .with(delegation::DelegationMcpModule::new(delegation_deps))?;
    Ok(
        spur_mcp::registry::legacy_brain_tool_registry_builder_from(builder)?
            .with(signals::SignalMcpModule::new(signal_deps))?
            .build(),
    )
}
