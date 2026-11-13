//! Facade of the `tantra-exec` bounded context (Hand).
//!
//! Re-exports the member crates under their short names so consumers import the
//! whole execution context from a single crate (facade-crate convention, root
//! plan 0003 Phase 4 / gap 0007).

pub use tantra_exec_angelone as angelone;
pub use tantra_exec_audit as audit;
pub use tantra_exec_auth as auth;
pub use tantra_exec_core as core;
pub use tantra_exec_router as router;
pub use tantra_exec_slicer as slicer;
pub use tantra_exec_upstox as upstox;
pub use tantra_exec_zerodha as zerodha;
