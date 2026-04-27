//! Spur-side ACP error types.
//!
//! `AcpError` carries variants that the typed ACP-call surface (e.g.
//! `set_session_model` on `NativeAcpConnection`) can report distinctly
//! from the agent's own wire failures. See spec §6.3 — the
//! `CapabilityMissing` variant fires when a session's `SpurAgentCaps`
//! advertises *neither* the dedicated method *nor* a viable fallback.

/// Errors emitted by the typed capability-aware ACP-call surface.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The session's `SpurAgentCaps` advertise neither the dedicated
    /// method nor a viable fallback for the named capability.
    #[error("agent capability missing: {0}")]
    CapabilityMissing(&'static str),
    /// Transport / wire failure surfaced from the underlying SDK.
    #[error(transparent)]
    Transport(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::AcpError;

    #[test]
    fn capability_missing_display_includes_name() {
        let e = AcpError::CapabilityMissing("set_model");
        let s = e.to_string();
        assert!(
            s.contains("set_model"),
            "Display impl must mention the missing capability name; got {s:?}"
        );
        assert!(
            s.to_lowercase().contains("capability"),
            "Display impl must mention 'capability'; got {s:?}"
        );
    }

    #[test]
    fn capability_missing_carries_static_str() {
        // Round-trip: variant tag + payload is preserved.
        let e = AcpError::CapabilityMissing("set_mode");
        match e {
            AcpError::CapabilityMissing(name) => assert_eq!(name, "set_mode"),
            other => panic!("expected CapabilityMissing variant, got {other:?}"),
        }
    }
}
