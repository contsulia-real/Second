use std::collections::BTreeMap;

use crate::legal_task_codec::{decode_legal_task, encode_legal_task};
use crate::payment::EstablishedTransfer;
use crate::prepared_plan::{PreparedOperation, PreparedTask, PreparedTaskPhase};
use crate::{
    AccountAddress, BftLocalState, BftValue, CurrencyAddress, PaymentAddress, PersistenceError,
    TaskId, ValidatorId,
};
use crate::{AddressRange, AddressRanges, ConsensusScope};

use super::codec::{Decoder, push_len, push_task_id};

pub(super) type VoteLocks = BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>;
pub(super) type BftStates = BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>;

pub(super) fn encode_local_state(
    out: &mut Vec<u8>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
    vote_locks: &VoteLocks,
    bft_states: &BftStates,
) -> Result<(), PersistenceError> {
    encode_prepared_tasks(out, prepared_tasks)?;

    push_len(out, vote_locks.len())?;
    for ((validator_id, scope), digest) in vote_locks {
        out.extend_from_slice(&validator_id.value().to_be_bytes());
        encode_scope(out, scope);
        out.extend_from_slice(digest);
    }

    push_len(out, bft_states.len())?;
    for ((validator_id, scope), state) in bft_states {
        out.extend_from_slice(&validator_id.value().to_be_bytes());
        encode_scope(out, scope);
        out.extend_from_slice(&state.validator_set_version().to_be_bytes());
        out.extend_from_slice(&state.round().to_be_bytes());
        encode_optional_lock(out, state.locked_round(), state.locked_digest());
        encode_optional_qc(out, state.valid_prevote_qc())?;
        encode_optional_bft_value(out, state.prevote());
        encode_optional_bft_value(out, state.precommit());
        encode_optional_lock(
            out,
            state.finality_ready_round(),
            state.finality_ready_digest(),
        );
        encode_optional_qc(out, state.finality_qc())?;
    }

    Ok(())
}

pub(super) fn decode_local_state(
    decoder: &mut Decoder<'_>,
) -> Result<(BTreeMap<TaskId, PreparedTask>, VoteLocks, BftStates), PersistenceError> {
    let prepared_tasks = decode_prepared_tasks(decoder)?;

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

    let bft_count = decoder.read_len()?;
    const MIN_BFT_STATE_SIZE: usize = 8 + 1 + 8 + 8 + 1 + 1 + 1 + 1;
    if bft_count > decoder.remaining() / MIN_BFT_STATE_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut bft_states = BTreeMap::new();
    for _ in 0..bft_count {
        let validator_id = ValidatorId::new(decoder.read_u64()?);
        let scope = decode_scope(decoder)?;
        let validator_set_version = decoder.read_u64()?;
        let round = decoder.read_u64()?;
        let (locked_round, locked_digest) = decode_optional_lock(decoder)?;
        if locked_round.is_some_and(|locked| locked > round) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let valid_prevote_qc = decode_optional_qc(decoder)?;
        let prevote = decode_optional_bft_value(decoder)?;
        let precommit = decode_optional_bft_value(decoder)?;
        let (finality_ready_round, finality_ready_digest) = decode_optional_lock(decoder)?;
        if finality_ready_round.is_some_and(|ready| ready > round) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let finality_qc = decode_optional_qc(decoder)?;

        let state = BftLocalState::from_persisted(
            validator_set_version,
            round,
            locked_round,
            locked_digest,
            valid_prevote_qc,
            prevote,
            precommit,
            finality_ready_round,
            finality_ready_digest,
            finality_qc,
        );
        if bft_states.insert((validator_id, scope), state).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok((prepared_tasks, vote_locks, bft_states))
}

fn encode_optional_qc(
    out: &mut Vec<u8>,
    certificate: Option<&crate::BftQuorumCertificate>,
) -> Result<(), PersistenceError> {
    match certificate {
        Some(certificate) => {
            out.push(1);
            let bytes = crate::encode_bft_network_message(
                &crate::BftNetworkMessage::QuorumCertificate(certificate.clone()),
            )
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(out, bytes.len())?;
            out.extend_from_slice(&bytes);
        }
        None => out.push(0),
    }
    Ok(())
}

fn decode_optional_qc(
    decoder: &mut Decoder<'_>,
) -> Result<Option<crate::BftQuorumCertificate>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => {
            let length = decoder.read_len()?;
            match crate::decode_bft_network_message(decoder.read_exact(length)?)
                .map_err(|_| PersistenceError::InvalidSnapshot)?
            {
                crate::BftNetworkMessage::QuorumCertificate(certificate) => Ok(Some(certificate)),
                _ => Err(PersistenceError::InvalidSnapshot),
            }
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn encode_optional_lock(out: &mut Vec<u8>, round: Option<u64>, digest: Option<[u8; 32]>) {
    match (round, digest) {
        (None, None) => out.push(0),
        (Some(round), Some(digest)) => {
            out.push(1);
            out.extend_from_slice(&round.to_be_bytes());
            out.extend_from_slice(&digest);
        }
        _ => unreachable!("BFT local state keeps round/digest pairs aligned"),
    }
}

fn decode_optional_lock(
    decoder: &mut Decoder<'_>,
) -> Result<(Option<u64>, Option<[u8; 32]>), PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok((None, None)),
        1 => Ok((Some(decoder.read_u64()?), Some(decoder.read_array_32()?))),
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn encode_optional_bft_value(out: &mut Vec<u8>, value: Option<BftValue>) {
    match value {
        None => out.push(0),
        Some(BftValue::Nil) => out.push(1),
        Some(BftValue::Digest(digest)) => {
            out.push(2);
            out.extend_from_slice(&digest);
        }
    }
}

fn decode_optional_bft_value(
    decoder: &mut Decoder<'_>,
) -> Result<Option<BftValue>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(BftValue::Nil)),
        2 => Ok(Some(BftValue::Digest(decoder.read_array_32()?))),
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn encode_prepared_operation(
    out: &mut Vec<u8>,
    operation: &PreparedOperation,
) -> Result<(), PersistenceError> {
    match operation {
        PreparedOperation::RegisterAccount { account } => {
            out.push(8);
            out.extend_from_slice(&account.bytes());
        }
        PreparedOperation::Issue { account, addresses } => {
            out.push(1);
            out.extend_from_slice(&account.bytes());
            encode_ranges(out, addresses)?;
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
            encode_ranges(out, currencies)?;
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
            encode_ranges(out, reserve)?;
            encode_ranges(out, replacement_reserve)?;
        }
        PreparedOperation::RegisterPaymentAddress { address, account } => {
            out.push(5);
            out.extend_from_slice(&address.bytes());
            out.extend_from_slice(&account.bytes());
        }
        PreparedOperation::RetirePaymentAddress { address } => {
            out.push(6);
            out.extend_from_slice(&address.bytes());
        }
        PreparedOperation::FinalizePaymentAddressRetirement { address } => {
            out.push(7);
            out.extend_from_slice(&address.bytes());
        }
    }
    Ok(())
}

fn decode_prepared_operation(
    decoder: &mut Decoder<'_>,
) -> Result<PreparedOperation, PersistenceError> {
    match decoder.read_u8()? {
        8 => Ok(PreparedOperation::RegisterAccount {
            account: AccountAddress::from_bytes(decoder.read_array_32()?),
        }),
        1 => Ok(PreparedOperation::Issue {
            account: AccountAddress::from_bytes(decoder.read_array_32()?),
            addresses: decode_ranges(decoder)?,
        }),
        2 => Ok(PreparedOperation::Transfer {
            transfer: EstablishedTransfer {
                source: PaymentAddress::from_bytes(decoder.read_array_32()?),
                destination: PaymentAddress::from_bytes(decoder.read_array_32()?),
                source_account: AccountAddress::from_bytes(decoder.read_array_32()?),
                destination_account: AccountAddress::from_bytes(decoder.read_array_32()?),
                amount: decoder.read_u64()?,
            },
            currencies: decode_ranges(decoder)?,
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
                reserve: decode_ranges(decoder)?,
                replacement_reserve: decode_ranges(decoder)?,
            })
        }
        5 => Ok(PreparedOperation::RegisterPaymentAddress {
            address: PaymentAddress::from_bytes(decoder.read_array_32()?),
            account: AccountAddress::from_bytes(decoder.read_array_32()?),
        }),
        6 => Ok(PreparedOperation::RetirePaymentAddress {
            address: PaymentAddress::from_bytes(decoder.read_array_32()?),
        }),
        7 => Ok(PreparedOperation::FinalizePaymentAddressRetirement {
            address: PaymentAddress::from_bytes(decoder.read_array_32()?),
        }),
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

fn encode_scope(out: &mut Vec<u8>, scope: &ConsensusScope) {
    match scope {
        ConsensusScope::CurrencyAllocation {
            validator_set_version,
            start,
        } => {
            out.push(5);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            out.extend_from_slice(&start.to_be_bytes());
        }
        ConsensusScope::PreparedTask(task_id) => {
            out.push(1);
            push_task_id(out, task_id);
        }
        ConsensusScope::PublicCheckpoint {
            validator_set_version,
            epoch,
        } => {
            out.push(2);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            out.extend_from_slice(&epoch.to_be_bytes());
        }
        ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version,
            serial,
        } => {
            out.push(4);
            out.extend_from_slice(&validator_set_version.to_be_bytes());
            out.extend_from_slice(&serial.to_be_bytes());
        }
    }
}

fn decode_scope(decoder: &mut Decoder<'_>) -> Result<ConsensusScope, PersistenceError> {
    match decoder.read_u8()? {
        5 => Ok(ConsensusScope::CurrencyAllocation {
            validator_set_version: decoder.read_u64()?,
            start: decoder.read_u64()?,
        }),
        1 => Ok(ConsensusScope::PreparedTask(decoder.read_task_id()?)),
        2 => Ok(ConsensusScope::PublicCheckpoint {
            validator_set_version: decoder.read_u64()?,
            epoch: decoder.read_u64()?,
        }),
        4 => Ok(ConsensusScope::StateRecoveryCheckpoint {
            validator_set_version: decoder.read_u64()?,
            serial: decoder.read_u64()?,
        }),
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

pub(super) fn encode_prepared_tasks(
    out: &mut Vec<u8>,
    prepared_tasks: &BTreeMap<TaskId, PreparedTask>,
) -> Result<(), PersistenceError> {
    push_len(out, prepared_tasks.len())?;
    for prepared in prepared_tasks.values() {
        encode_prepared_task(out, prepared)?;
    }

    Ok(())
}

pub(super) fn decode_prepared_tasks(
    decoder: &mut Decoder<'_>,
) -> Result<BTreeMap<TaskId, PreparedTask>, PersistenceError> {
    let task_count = decoder.read_len()?;
    const MIN_PREPARED_TASK_SIZE: usize = 1 + 1 + 32 + 8 + 1 + 8;
    if task_count > decoder.remaining() / MIN_PREPARED_TASK_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let mut prepared_tasks = BTreeMap::new();
    for _ in 0..task_count {
        let task_id = decoder.read_task_id()?;
        let request_digest = decoder.read_array_32()?;
        let source_len = decoder.read_len()?;
        let source_task = decode_legal_task(decoder.read_exact(source_len)?)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        let validator_set_version = decoder.read_u64()?;
        let phase = match decoder.read_u8()? {
            1 => PreparedTaskPhase::Prepared,
            2 => PreparedTaskPhase::Voting,
            3 => PreparedTaskPhase::Finalized,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let (commit_authorized, conflict_abort) = match decoder.read_u8()? {
            0 => (true, false),
            1 => (true, true),
            2 => (false, false),
            3 => (false, true),
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let finality_votes = match decoder.read_u8()? {
            0 => None,
            1 => {
                let vote_count = decoder.read_len()?;
                const FINALITY_VOTE_SIZE: usize = 8 + 64;
                if vote_count > decoder.remaining() / FINALITY_VOTE_SIZE {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                let mut votes = Vec::with_capacity(vote_count);
                for _ in 0..vote_count {
                    votes.push(
                        crate::finality_codec::decode_validator_vote(
                            decoder
                                .read_exact(crate::finality_codec::ENCODED_VALIDATOR_VOTE_SIZE)?,
                        )
                        .ok_or(PersistenceError::InvalidSnapshot)?,
                    );
                }
                Some(votes)
            }
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

        let mut prepared = PreparedTask::from_persisted(
            task_id.clone(),
            request_digest,
            source_task,
            validator_set_version,
            phase,
            finality_votes,
            operations,
        );
        prepared.commit_authorized = commit_authorized;
        prepared.conflict_abort = conflict_abort;
        let variant_count = decoder.read_len()?;
        // Every variant contains rights and an operation-count field. Bound
        // allocation by available bytes before decoding the actual operations.
        if variant_count > decoder.remaining() / 9 {
            return Err(PersistenceError::InvalidSnapshot);
        }
        for _ in 0..variant_count {
            let commit_authorized = match decoder.read_u8()? {
                0 => false,
                1 => true,
                _ => return Err(PersistenceError::InvalidSnapshot),
            };
            let operation_count = decoder.read_len()?;
            if operation_count != prepared.operations.len()
                || operation_count > decoder.remaining() / MIN_PREPARED_OPERATION_SIZE
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let mut operations = Vec::with_capacity(operation_count);
            for _ in 0..operation_count {
                operations.push(decode_prepared_operation(decoder)?);
            }
            prepared
                .variants
                .push(crate::prepared_plan::PreparedVariant {
                    operations,
                    commit_authorized,
                });
        }
        if prepared_tasks.insert(task_id.clone(), prepared).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok(prepared_tasks)
}

pub(super) fn encode_prepared_task(
    out: &mut Vec<u8>,
    prepared: &PreparedTask,
) -> Result<(), PersistenceError> {
    push_task_id(out, &prepared.task_id);
    out.extend_from_slice(&prepared.request_digest);
    let source =
        encode_legal_task(&prepared.source_task).map_err(|_| PersistenceError::InvalidSnapshot)?;
    push_len(out, source.len())?;
    out.extend_from_slice(&source);
    out.extend_from_slice(&prepared.validator_set_version.to_be_bytes());
    out.push(match prepared.phase {
        PreparedTaskPhase::Prepared => 1,
        PreparedTaskPhase::Voting => 2,
        PreparedTaskPhase::Finalized => 3,
    });
    out.push(u8::from(prepared.conflict_abort) | (u8::from(!prepared.commit_authorized) << 1));
    match &prepared.finality_votes {
        Some(votes) => {
            out.push(1);
            push_len(out, votes.len())?;
            for vote in votes {
                crate::finality_codec::encode_validator_vote(out, vote);
            }
        }
        None => out.push(0),
    }

    push_len(out, prepared.operations.len())?;
    for operation in &prepared.operations {
        encode_prepared_operation(out, operation)?;
    }
    push_len(out, prepared.variants.len())?;
    for variant in &prepared.variants {
        out.push(u8::from(variant.commit_authorized));
        push_len(out, variant.operations.len())?;
        for operation in &variant.operations {
            encode_prepared_operation(out, operation)?;
        }
    }
    Ok(())
}

fn encode_ranges(out: &mut Vec<u8>, ranges: &AddressRanges) -> Result<(), PersistenceError> {
    push_len(out, ranges.ranges().len())?;
    for range in ranges.ranges() {
        out.extend_from_slice(&range.start.value().to_be_bytes());
        out.extend_from_slice(&range.len.to_be_bytes());
    }
    Ok(())
}
fn decode_ranges(decoder: &mut Decoder<'_>) -> Result<AddressRanges, PersistenceError> {
    let count = decoder.read_len()?;
    if count > decoder.remaining() / 16 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut ranges = Vec::with_capacity(count);
    for _ in 0..count {
        ranges.push(
            AddressRange::new(
                CurrencyAddress::new(decoder.read_u64()?),
                decoder.read_u64()?,
            )
            .ok_or(PersistenceError::InvalidSnapshot)?,
        );
    }
    AddressRanges::from_canonical(ranges).ok_or(PersistenceError::InvalidSnapshot)
}
