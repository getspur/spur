pub mod guard;
pub mod ledger;
pub mod limits;
pub mod prompt_builder;
pub mod router;

use std::sync::Arc;

pub use guard::{run_reconciler_loop, GuardOutcome, PeerMessageGuard, StrandedMessage};
pub use ledger::{InMemoryLedger, LedgerEntry, LedgerError, PeerMailboxLedger};
pub use limits::Limits;
pub use router::{PeerMailboxRouter, RouterError};

#[derive(Clone)]
pub struct PeerMailboxBundle {
    pub router: Arc<router::PeerMailboxRouter>,
    pub builder: Arc<prompt_builder::PeerPromptContextBuilder>,
    pub ledger: Arc<dyn ledger::PeerMailboxLedger>,
}
