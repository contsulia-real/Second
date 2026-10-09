//! Bounded query data derived once from one immutable private snapshot.
use super::*;
use crate::payment::PaymentExecution;
use crate::{
    AccountAddress, CurrencyAddress, OperationClaimId, PaymentAddress, PaymentAddressStatus,
};

struct TransferRow {
    id: OperationClaimId,
    execution: PaymentExecution,
    incoming: bool,
    outgoing: bool,
}
enum Rows {
    Balance,
    Addresses(Vec<(PaymentAddress, PaymentAddressStatus)>),
    Transfers(Vec<TransferRow>),
    Currencies(Vec<CurrencyAddress>),
}
pub(super) struct Projection {
    account: AccountAddress,
    kind: u8,
    generation: u64,
    validator_set_version: u64,
    exists: bool,
    balance: u64,
    cursor: u64,
    rows: Rows,
    _permit: Permit,
}

pub(super) fn push_bounded<T>(
    rows: &mut Vec<T>,
    row: T,
    retained_bytes: usize,
) -> Result<(), NetworkError> {
    if rows.len() == MAX_ACCOUNT_QUERY_ROWS {
        return Err(denied("budget_exceeded"));
    }
    if rows.len() == rows.capacity() {
        let capacity = (rows.capacity().max(2) * 2).min(MAX_ACCOUNT_QUERY_ROWS);
        if capacity * size_of::<T>() + retained_bytes
            > MAX_PROJECTION_BYTES - size_of::<Projection>()
        {
            return Err(denied("budget_exceeded"));
        }
        rows.try_reserve_exact(capacity - rows.len())
            .map_err(|_| denied("budget_exceeded"))?;
    }
    if rows.capacity() * size_of::<T>() + retained_bytes
        > MAX_PROJECTION_BYTES - size_of::<Projection>()
    {
        return Err(denied("budget_exceeded"));
    }
    rows.push(row);
    Ok(())
}

impl Projection {
    pub(super) fn build(
        binding: [u8; 32],
        message: &NetworkMessage,
        loader: Option<SnapshotLoader>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self, NetworkError> {
        authenticate(binding, message).map_err(|_| denied("unauthorized"))?;
        let NetworkMessage::AccountQuery {
            account,
            kind,
            cursor: 0,
            generation: None,
            ..
        } = message
        else {
            return Err(denied("invalid_query"));
        };
        check_work(deadline, cancelled)?;
        let permit = Permit::acquire()?;
        // ponytail: synchronous reads cannot cancel in flight; a cancellable loader is needed to bound disk stalls.
        let snapshot = loader.ok_or_else(|| denied("unavailable"))?()?;
        check_work(deadline, cancelled)?;
        let address = AccountAddress::from_bytes(*account);
        let state = &snapshot.state;
        let mut rows = match kind {
            1 => Rows::Balance,
            2 => Rows::Addresses(Vec::new()),
            3 => Rows::Transfers(Vec::new()),
            4 => Rows::Currencies(Vec::new()),
            _ => return Err(denied("invalid_query")),
        };
        let mut balance = 0;
        for (index, (currency, record)) in state.business.currencies.iter().enumerate() {
            if index.is_multiple_of(4096) {
                check_work(deadline, cancelled)?;
            }
            if record.owner == Some(address) {
                balance += 1;
                if let Rows::Currencies(values) = &mut rows {
                    push_bounded(values, *currency, 0)?;
                }
            }
        }
        if let Rows::Addresses(values) = &mut rows {
            for (index, (payment, record)) in state.business.payment_addresses.iter().enumerate() {
                if index.is_multiple_of(4096) {
                    check_work(deadline, cancelled)?;
                }
                if record.account == address {
                    push_bounded(values, (*payment, record.status), 0)?;
                }
            }
        }
        if let Rows::Transfers(values) = &mut rows {
            let mut retained_bytes = 0;
            for (index, (id, execution)) in state.business.payment_history.iter().enumerate() {
                if index.is_multiple_of(4096) {
                    check_work(deadline, cancelled)?;
                }
                let incoming =
                    state.payment_address_account(execution.destination) == Some(address);
                let outgoing = state.payment_address_account(execution.source) == Some(address);
                if incoming || outgoing {
                    // Charge even shared TaskId bytes so releasing the snapshot cannot hide retained memory.
                    retained_bytes += id.task_id().len() + 2 * size_of::<usize>();
                    push_bounded(
                        values,
                        TransferRow {
                            id: id.clone(),
                            execution: execution.clone(),
                            incoming,
                            outgoing,
                        },
                        retained_bytes,
                    )?;
                }
            }
        }
        check_work(deadline, cancelled)?;
        Ok(Self {
            account: address,
            kind: *kind,
            generation: snapshot.generation,
            validator_set_version: snapshot.validator_set.version(),
            exists: state.has_account(address),
            balance,
            cursor: 0,
            rows,
            _permit: permit,
        })
    }

    pub(super) fn page_authenticated(
        &mut self,
        message: &NetworkMessage,
    ) -> Result<AccountView, NetworkError> {
        let NetworkMessage::AccountQuery {
            account,
            kind,
            cursor,
            generation,
            nonce,
            ..
        } = message
        else {
            return Err(denied("invalid_query"));
        };
        if AccountAddress::from_bytes(*account) != self.account
            || *kind != self.kind
            || *cursor != self.cursor
            || (*cursor > 0 && *generation != Some(self.generation))
        {
            return Err(denied("invalid_query"));
        }
        let total = match &self.rows {
            Rows::Balance => 0,
            Rows::Addresses(rows) => rows.len(),
            Rows::Transfers(rows) => rows.len(),
            Rows::Currencies(rows) => rows.len(),
        };
        let start = *cursor as usize;
        let end = (start + usize::from(MAX_ACCOUNT_QUERY_PAGE)).min(total);
        if start > total {
            return Err(denied("invalid_query"));
        }
        let mut view = AccountView {
            account: self.account.to_string(),
            kind: self.kind,
            cursor: *cursor,
            generation: self.generation,
            validator_set_version: self.validator_set_version,
            exists: self.exists,
            balance: self.balance,
            total: total as u64,
            addresses: Vec::new(),
            transfers: Vec::new(),
            currencies: Vec::new(),
            next: (end < total).then_some(end as u64),
            nonce: nonce.to_vec(),
        };
        match &self.rows {
            Rows::Balance => {}
            Rows::Addresses(rows) => {
                view.addresses = rows[start..end]
                    .iter()
                    .map(|(address, status)| AccountPaymentAddress {
                        address: address.to_string(),
                        status: match status {
                            PaymentAddressStatus::Active => "active",
                            PaymentAddressStatus::Retiring => "retiring",
                            PaymentAddressStatus::Retired => "retired",
                        }
                        .to_owned(),
                    })
                    .collect()
            }
            Rows::Transfers(rows) => {
                view.transfers = rows[start..end]
                    .iter()
                    .map(|row| AccountTransfer {
                        task_id: row.id.task_id().to_string(),
                        operation_index: row.id.operation_index(),
                        source: row.execution.source.to_string(),
                        destination: row.execution.destination.to_string(),
                        amount: row.execution.amount,
                        incoming: row.incoming,
                        outgoing: row.outgoing,
                    })
                    .collect()
            }
            Rows::Currencies(rows) => {
                view.currencies = rows[start..end].iter().map(ToString::to_string).collect()
            }
        }
        self.cursor = end as u64;
        Ok(view)
    }
}
