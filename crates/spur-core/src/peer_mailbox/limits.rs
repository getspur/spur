/// Stage-1 safety limits. Expanded by Task 9 (tiered limits).
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_peer_message_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // Per spec: 4 KiB body cap.
        Self {
            max_peer_message_size: 4 * 1024,
        }
    }
}
