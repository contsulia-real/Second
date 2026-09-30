use std::collections::BTreeMap;

use crate::payment::EstablishedTransfer;
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::validator_signer::FinalityScope;
use crate::{
    AccountAddress, CurrencyAddress, PaymentAddress, PersistenceError, TaskId, ValidatorId,
};

use super::codec::{Decoder, push_len, push_task_id};

pub(super) type VoteLocks = BTreeMap<(ValidatorId, FinalityScope), [u8; 32]>;

pub(super) fn encode_local_state(
    out: &mut Vec<u8>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    vote_locks: &VoteLocks,
) -> Result<(), PersistenceError> {
    push_len(out, prepared_tasks.len())?;
    for prepared in prepared_tasks.values() {
        push_task_id(out, &prepared.task_id);
        out.extend_from_slice(&prepared.request_digest);
        out.extend_from_slice(&prepared.validator_set_version.to_be_bytes());
        out.push(match prepared.phase {
            PreparedTaskPhase::Prepared => 1,
            PreparedTaskPhase::Voting => 2,
            PreparedTaskPhase::Finalized => 3,
        });

        push_len(out, prepared.operations.len())?;
        for operation in &prepared.operations {
            encode_prepared_operation(out, operation)?;
        }
    }

    push_len(out, vote_locks.len())?;
    for ((validator_id, scope), digest) in vote_locks {
        out.extend_from_slice(&validator_id.value().to_be_bytes());
        encode_scope(out, scope);
        out.extend_from_slice(digest);
    }

    Ok(())
}

pub(super) fn decode_local_state(
    decoder: &mut Decoder<'_>,
) -> Result<(BTreeMap<TaskId, PreparedTask>, VoteLocks), PersistenceError> {
    let task_count = decoder.read_len()?;
    const MIN_PREPARED_TASK_SIZE: usize = 1 + 1 + 32 + 8 + 1 + 8;
    if task_count > decoder.remaining() / MIN_PREPARED_TASK_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut prepared_tasks = BTreeMap::new();
    for _ in 0..task_count {
        let task_id = decoder.read_task_id()?;
        let request_digest = decoder.read_array_32()?;
        let validator_set_version = decoder.read_u64()?;
        let phase = match decoder.read_u8()? {
            1 => PreparedTaskPhase::Prepared,
            2 => PreparedTaskPhase::Voting,
            3 => PreparedTaskPhase::Finalized,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let operation_count = decoder.read_len()?;
        const MIN_PREPARED_OPERATION_SIZE: usize = 1 + 8;
        if operation_count > decoder.remaining() / MIN_PREPARED_OPERATION_SIZE {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let mut operations = Vec::with_capacity(operation_count);
        for _ in 0..operation_count {
            operations.push(decode_prepared_operation(decoder)?);
        }

        if prepared_tasks
            .insert(
                task_id.clone(),
                PreparedTask::from_persisted(
                    task_id,
                    request_digest,
                    validator_set_version,
                    phase,
                    operations,
                ),
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let lock_count = decoder.read_len()?;
    const MIN_VOTE_LOCK_SIZE: usize = 8 + 1 + 1 + 1 + 32;
    if lock_count > decoder.remaining() / MIN_VOTE_LOCK_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut vote_locks = BTreeMap::new();
    for _ in 0..lock_count {
        let validator_id = ValidatorId::new(decoder.read_u64()?);
        let scope = decode_scope(decoder)?;
        let digest = decoder.read_array_32()?;
        if vote_locks.insert((validator_id, scope), digest).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok((prepared_tasks, vote_locks))
}

fn encode_prepared_operation(
    out: &mut Vec<u8>,
    operation: &PreparedOperation,
) -> Result<(), PersistenceError> {
    match operation {
        PreparedOperation::Issue { account, addresses } => {
            out.push(1);
            out.extend_from_slice(&account.bytes());
            encode_addresses(out, addresses)?;
        }
        PreparedOperation::Transfer {
            transfer,
            currencies,
        } => {
            out.push(2);
            out.extend_from_slice(&transfer.source.bytes());
            out.extend_from_slice(&transfer.destination.bytes());
            out.extend_from_slice(&transfer.source_account.bytes());
            out.extend_from_slice(&transfer.destination_account.bytes());
            out.extend_from_slice(&transfer.amount.to_be_bytes());
            encode_addresses(out, currencies)?;
        }
        PreparedOperation::Destroy { currencies } => {
            out.push(3);
            encode_addresses(out, currencies)?;
        }
        PreparedOperation::LeakRepair {
            leaked,
            leaked_owners,
            reserve,
            replacement_reserve,
        } => {
            out.push(4);
            encode_addresses(out, leaked)?;
            push_len(out, leaked_owners.len())?;
            for owner in leaked_owners {
                out.extend_from_slice(&owner.bytes());
            }
            encode_addresses(out, reserve)?;
            encode_addresses(out, replacement_reserve)?;
        }
    }
    Ok(())
}

fn decode_prepared_operation(
    decoder: &mut Decoder<'_>,
) -> Result<PreparedOperation, PersistenceError> {
    match decoder.read_u8()? {
        1 => Ok(PreparedOperation::Issue {
            account: AccountAddress::from_bytes(decoder.read_array_32()?),
            addresses: decode_addresses(decoder)?,
        }),
        2 => Ok(PreparedOperation::Transfer {
            transfer: EstablishedTransfer {
                source: PaymentAddress::from_bytes(decoder.read_array_32()?),
                destination: PaymentAddress::from_bytes(decoder.read_array_32()?),
                source_account: AccountAddress::from_bytes(decoder.read_array_32()?),
                destination_account: AccountAddress::from_bytes(decoder.read_array_32()?),
                amount: decoder.read_u64()?,
            },
            currencies: decode_addresses(decoder)?,
        }),
        3 => Ok(PreparedOperation::Destroy {
            currencies: decode_addresses(decoder)?,
        }),
        4 => {
            let leaked = decode_addresses(decoder)?;
            let owner_count = decoder.read_len()?;
            if owner_count > decoder.remaining() / 32 {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let mut leaked_owners = Vec::with_capacity(owner_count);
            for _ in 0..owner_count {
                leaked_owners.push(AccountAddress::from_bytes(decoder.read_array_32()?));
            }

            Ok(PreparedOperation::LeakRepair {
                leaked,
                leaked_owners,
                reserve: decode_addresses(decoder)?,
                replacement_reserve: decode_addresses(decoder)?,
            })
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn encode_addresses(
    out: &mut Vec<u8>,
    addresses: &[CurrencyAddress],
) -> Result<(), PersistenceError> {
    push_len(out, addresses.len())?;
    for address in addresses {
        out.extend_from_slice(&address.value().to_be_bytes());
    }
    Ok(())
}

fn decode_addresses(decoder: &mut Decoder<'_>) -> Result<Vec<CurrencyAddress>, PersistenceError> {
    let count = decoder.read_len()?;
    if count > decoder.remaining() / 8 {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut addresses = Vec::with_capacity(count);
    for _ in 0..count {
        addresses.push(CurrencyAddress::new(decoder.read_u64()?));
    }
    Ok(addresses)
}

fn encode_scope(out: &mut Vec<u8>, scope: &FinalityScope) {
    match scope {
        FinalityScope::PreparedTask(task_id) => {
            out.push(1);
            push_task_id(out, task_id);
        }
        FinalityScope::PublicCheckpoint(epoch) => {
            out.push(2);
            out.extend_from_slice(&epoch.to_be_bytes());
        }
        FinalityScope::ValidatorSetTransition {
            current_validator_set_version,
            activation_epoch,
        } => {
            out.push(3);
            out.extend_from_slice(&current_validator_set_version.to_be_bytes());
            out.extend_from_slice(&activation_epoch.to_be_bytes());
        }
    }
}

fn decode_scope(decoder: &mut Decoder<'_>) -> Result<FinalityScope, PersistenceError> {
    match decoder.read_u8()? {
        1 => Ok(FinalityScope::PreparedTask(decoder.read_task_id()?)),
        2 => Ok(FinalityScope::PublicCheckpoint(decoder.read_u64()?)),
        3 => Ok(FinalityScope::ValidatorSetTransition {
            current_validator_set_version: decoder.read_u64()?,
            activation_epoch: decoder.read_u64()?,
        }),
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}
