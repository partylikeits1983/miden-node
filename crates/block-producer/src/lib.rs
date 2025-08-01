#![recursion_limit = "256"]
use std::num::NonZeroUsize;

#[cfg(test)]
pub mod test_utils;

mod batch_builder;
mod block_builder;
mod domain;
mod mempool;
pub mod store;

#[cfg(feature = "testing")]
pub mod errors;
#[cfg(not(feature = "testing"))]
mod errors;

pub mod server;
pub use server::BlockProducer;

// CONSTANTS
// =================================================================================================

/// The name of the block producer component.
pub const COMPONENT: &str = "miden-block-producer";

/// The number of transactions per batch.
pub const SERVER_MAX_TXS_PER_BATCH: usize = 8;

/// Maximum number of batches per block.
pub const SERVER_MAX_BATCHES_PER_BLOCK: usize = 8;

/// Size of the batch building worker pool.
const SERVER_NUM_BATCH_BUILDERS: NonZeroUsize = NonZeroUsize::new(2).unwrap();

/// The number of blocks of committed state that the mempool retains.
///
/// This determines the grace period incoming transactions have between fetching their input from
/// the store and verification in the mempool.
const SERVER_MEMPOOL_STATE_RETENTION: usize = 5;

/// Transactions are rejected by the mempool if there is less than this amount of blocks between the
/// chain tip and the transaction's expiration block.
///
/// This rejects transactions which would likely expire before making it into a block.
const SERVER_MEMPOOL_EXPIRATION_SLACK: u32 = 2;

const _: () = assert!(
    SERVER_MAX_BATCHES_PER_BLOCK <= miden_objects::MAX_BATCHES_PER_BLOCK,
    "Server constraint cannot exceed the protocol's constraint"
);

const _: () = assert!(
    SERVER_MAX_TXS_PER_BATCH <= miden_objects::MAX_ACCOUNTS_PER_BATCH,
    "Server constraint cannot exceed the protocol's constraint"
);

/// An extension trait used only locally to implement telemetry injection.
trait TelemetryInjectorExt {
    /// Inject [`tracing`] telemetry from self.
    fn inject_telemetry(&self);
}
