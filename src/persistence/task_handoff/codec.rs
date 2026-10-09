//! Canonical handoff encoding and commitment.
use super::super::codec::{Decoder, push_len};
use super::super::local_codec::{decode_prepared_tasks, encode_prepared_task};
use super::*;

impl TaskHandoff {
    pub(crate) fn digest(&self) -> Result<[u8; 32], PersistenceError> {
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        let mut header = Vec::new();
        self.encode_certifier(&mut header)?;
        self.encode_business_digest(&mut header);
        hash.update(&header);
        hash.update((self.plans.len() as u64).to_be_bytes());
        let mut buffer = Vec::new();
        let mut total = header.len() + 16 + self.business_baseline_bytes().len();
        if total > MAX_HANDOFF_SIZE {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        for plan in self.plans.values() {
            buffer.clear();
            // The map count is part of the reused codec framing.
            push_len(&mut buffer, 1)?;
            encode_prepared_task(&mut buffer, plan)?;
            total = total
                .checked_add(buffer.len())
                .ok_or(PersistenceError::SnapshotTooLarge)?;
            if total > MAX_HANDOFF_SIZE {
                return Err(PersistenceError::SnapshotTooLarge);
            }
            hash.update(&buffer);
        }
        buffer.clear();
        self.encode_requests(&mut buffer)?;
        if total
            .checked_add(buffer.len())
            .is_none_or(|size| size > MAX_HANDOFF_SIZE)
        {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        hash.update(&buffer);
        Ok(hash.finalize().into())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, PersistenceError> {
        Ok(self.encode_shared()?.as_ref().clone())
    }

    pub(crate) fn encode_shared(&self) -> Result<std::sync::Arc<Vec<u8>>, PersistenceError> {
        if let Some(bytes) = self.encoded.get() {
            return Ok(bytes.clone());
        }
        let mut bytes = Vec::new();
        self.encode_certifier(&mut bytes)?;
        self.encode_business_digest(&mut bytes);
        let baseline = self.business_baseline_bytes();
        push_len(&mut bytes, baseline.len())?;
        bytes.extend_from_slice(baseline);
        if bytes.len() > MAX_HANDOFF_SIZE {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        push_len(&mut bytes, self.plans.len())?;
        for plan in self.plans.values() {
            // Reuse the sole frozen-plan codec, including its operation encoding.
            push_len(&mut bytes, 1)?;
            encode_prepared_task(&mut bytes, plan)?;
            if bytes.len() > MAX_HANDOFF_SIZE {
                return Err(PersistenceError::SnapshotTooLarge);
            }
        }
        self.encode_requests(&mut bytes)?;
        let bytes = std::sync::Arc::new(bytes);
        let _ = self.encoded.set(bytes.clone());
        Ok(self.encoded.get().cloned().unwrap_or(bytes))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        if bytes.len() > MAX_HANDOFF_SIZE {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        let mut reader = Decoder::new(bytes);
        let certifier_set = match reader.read_u8()? {
            0 => None,
            1 => Some(super::super::validator_codec::decode_validator_set(
                &mut reader,
            )?),
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let business_digest = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_exact(32)?.try_into().unwrap()),
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let baseline_len = reader.read_len()?;
        let baseline = reader.read_exact(baseline_len)?;
        let business_baseline = match business_digest {
            None if baseline.is_empty() => None,
            Some(expected)
                if !baseline.is_empty() && hash_business_baseline(baseline) == expected =>
            {
                super::super::codec::decode_handoff_business_baseline(baseline)?;
                Some(std::sync::Arc::new(baseline.to_vec()))
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let count = reader.read_len()?;
        if count > reader.remaining() / 64 {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let mut value = Self {
            certifier_set,
            business_digest,
            business_baseline,
            ..Self::default()
        };
        if (count != 0 || value.business_digest.is_some()) && value.certifier_set.is_none() {
            return Err(PersistenceError::InvalidSnapshot);
        }
        for _ in 0..count {
            let plans = decode_prepared_tasks(&mut reader)?;
            if plans.len() != 1 {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let plan = plans.into_values().next().unwrap();
            if plan.phase != PreparedTaskPhase::Prepared
                || !plan.commit_authorized
                || plan.conflict_abort
                || plan.finality_votes.is_some()
                || !plan.variants.is_empty()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let before = value.plans.len();
            value.insert(plan)?;
            if value.plans.len() != before + 1 {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        let request_count = reader.read_len()?;
        if request_count > reader.remaining() / 8 {
            return Err(PersistenceError::InvalidSnapshot);
        }
        for _ in 0..request_count {
            let length = reader.read_len()?;
            let task = crate::legal_task_codec::decode_legal_task(reader.read_exact(length)?)
                .ok_or(PersistenceError::InvalidSnapshot)?;
            let before = value.requests.len();
            value.insert_request(task)?;
            if value.requests.len() != before + 1 {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        if value.requires_commitment() && value.certifier_set.is_none() {
            return Err(PersistenceError::InvalidSnapshot);
        }
        reader.finish()?;
        Ok(value)
    }

    fn encode_certifier(&self, out: &mut Vec<u8>) -> Result<(), PersistenceError> {
        match &self.certifier_set {
            None => out.push(0),
            Some(set) => {
                out.push(1);
                super::super::validator_codec::encode_validator_set(out, set)?;
            }
        }
        Ok(())
    }

    fn encode_requests(&self, out: &mut Vec<u8>) -> Result<(), PersistenceError> {
        push_len(out, self.requests.len())?;
        for (task_id, task) in &self.requests {
            if task.payload().task_id() != *task_id || self.task_context(task_id).is_some() {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let bytes = crate::legal_task_codec::encode_legal_task(task)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(out, bytes.len())?;
            out.extend_from_slice(&bytes);
            if out.len() > MAX_HANDOFF_SIZE {
                return Err(PersistenceError::SnapshotTooLarge);
            }
        }
        if out.len() > MAX_HANDOFF_SIZE {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        Ok(())
    }

    fn encode_business_digest(&self, out: &mut Vec<u8>) {
        out.push(u8::from(self.business_digest.is_some()));
        if let Some(digest) = self.business_digest {
            out.extend_from_slice(&digest);
        }
    }
}
