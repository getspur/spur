pub mod signals;

pub fn brain_tool_registry(
    signal_deps: signals::SignalMcpDeps,
) -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    Ok(spur_mcp::registry::legacy_brain_tool_registry_builder()?
        .with(signals::SignalMcpModule::new(signal_deps))?
        .build())
}
