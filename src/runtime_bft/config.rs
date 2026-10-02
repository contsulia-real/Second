use std::sync::Arc;

use crate::{AuthorizerSet, BftTimeoutConfig};

#[derive(Clone)]
pub struct ValidatorRuntimeConfig {
    authorizers: AuthorizerSet,
    bft_timeouts: BftTimeoutConfig,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl ValidatorRuntimeConfig {
    pub fn new<F>(authorizers: AuthorizerSet, bft_timeouts: BftTimeoutConfig, now: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        Self {
            authorizers,
            bft_timeouts,
            now: Arc::new(now),
        }
    }

    pub(crate) fn authorizers(&self) -> &AuthorizerSet {
        &self.authorizers
    }

    pub(crate) const fn bft_timeouts(&self) -> BftTimeoutConfig {
        self.bft_timeouts
    }

    pub(crate) fn now(&self) -> u64 {
        (self.now)()
    }
}
