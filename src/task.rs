use crate::{AccountAddress, CurrencyAddress, TaskId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    Transfer {
        source: AccountAddress,
        destination: AccountAddress,
        amount: u64,
    },
    Issue {
        account: AccountAddress,
        count: u64,
    },
    Destroy {
        currencies: Vec<CurrencyAddress>,
    },
    LeakRepair {
        leaked: Vec<CurrencyAddress>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegalTask {
    task_id: TaskId,
    request_digest: [u8; 32],
    expires_at: Option<u64>,
    operations: Vec<Operation>,
}

impl LegalTask {
    pub fn new(
        task_id: TaskId,
        request_digest: [u8; 32],
        expires_at: Option<u64>,
        operations: Vec<Operation>,
    ) -> Self {
        Self {
            task_id,
            request_digest,
            expires_at,
            operations,
        }
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    pub const fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
}
