//! The system's clock and randomness, for dynamic variables.
//!
//! The smallest adapter in the project, and it exists for the same reason
//! [`crate::secrets::EnvSecretProvider`] does: reading the clock is touching
//! the world, and the domain does not do that. Isolating it here is what lets a
//! test pin the seed and check that `{{$timestamp}}` comes out exact.

use http_studio_application::DynamicSource;
use http_studio_domain::DynamicSeed;

/// A seed taken from the system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDynamicSource;

impl SystemDynamicSource {
    /// Creates the provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl DynamicSource for SystemDynamicSource {
    fn seed(&self) -> DynamicSeed {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));

        // The instant's nanoseconds stand in for randomness. This is not a
        // cryptographic generator and does not pretend to be: `{{$randomInt}}`
        // and `{{$uuid}}` generate test data, never secrets, and saying so here
        // is what keeps someone from using them for the latter.
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()));

        DynamicSeed {
            unix_seconds: now,
            random: jitter ^ (now.cast_unsigned().rotate_left(17)),
        }
    }
}
