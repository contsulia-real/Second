use crate::{
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, ValidatorAdmissionError,
    ValidatorAdmissionRequest, ValidatorConsensusKeyRotationRequest, ValidatorCredential,
    ValidatorId, ValidatorRegistry, ValidatorRotationAuthority, ValidatorSet, ValidatorSetError,
    ValidatorSetTransition, ValidatorTransitionError, ValidatorVote,
};

pub const MAX_VALIDATOR_TRANSITION_SOURCE_SIZE: usize = 48 * 1024;
pub const MAX_VALIDATOR_TRANSITION_PROOF_SIZE: usize = 60 * 1024;
const ENCODED_TRANSITION_VOTE_SIZE: usize = 8 + 64;
const CREDENTIAL_SIZE: usize = 8 + 32 * 3;
const ADMISSION_SIZE: usize = 4 + CREDENTIAL_SIZE + 64 * 3;
const ROTATION_SIZE: usize = 4 + 1 + 8 + 8 + 32 + 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSetTransitionSource {
    protocol_version: u32,
    currency_frontier: u64,
    next_validator_set: ValidatorSet,
    admissions: Vec<ValidatorAdmissionRequest>,
    consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
    handoff_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSetTransitionProof {
    source: ValidatorSetTransitionSource,
    votes: Vec<ValidatorVote>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorTransitionSourceError {
    Admission(ValidatorAdmissionError),
    Transition(ValidatorTransitionError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorTransitionSourceCodecError {
    InvalidLength,
    TooLarge,
    InvalidValidatorSet(ValidatorSetError),
    InvalidAdmission(ValidatorAdmissionError),
    InvalidRotationAuthority,
}

impl ValidatorSetTransitionProof {
    pub fn from_certified(certified: &CertifiedValidatorSetTransition) -> Self {
        Self {
            source: ValidatorSetTransitionSource::from_transition(certified.transition()),
            votes: certified.certificate().votes().to_vec(),
        }
    }

    pub fn source(&self) -> &ValidatorSetTransitionSource {
        &self.source
    }

    pub fn votes(&self) -> &[ValidatorVote] {
        &self.votes
    }

    pub fn verify(
        &self,
        current_validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<CertifiedValidatorSetTransition, ValidatorTransitionSourceError> {
        let transition = self
            .source
            .verify(current_validator_set, validator_registry)?;
        CertifiedValidatorSetTransition::new(transition, self.votes.clone(), current_validator_set)
            .map_err(ValidatorTransitionSourceError::Transition)
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>, ValidatorTransitionSourceCodecError> {
        let source = self.source.encode_bytes()?;
        let source_len = u16::try_from(source.len())
            .map_err(|_| ValidatorTransitionSourceCodecError::TooLarge)?;
        let vote_count = u16::try_from(self.votes.len())
            .map_err(|_| ValidatorTransitionSourceCodecError::TooLarge)?;
        let encoded_len = 2_usize
            .checked_add(source.len())
            .and_then(|len| len.checked_add(2))
            .and_then(|len| {
                self.votes
                    .len()
                    .checked_mul(ENCODED_TRANSITION_VOTE_SIZE)
                    .and_then(|vote_bytes| len.checked_add(vote_bytes))
            })
            .ok_or(ValidatorTransitionSourceCodecError::TooLarge)?;
        if encoded_len > MAX_VALIDATOR_TRANSITION_PROOF_SIZE {
            return Err(ValidatorTransitionSourceCodecError::TooLarge);
        }

        let mut out = Vec::with_capacity(encoded_len);
        out.extend_from_slice(&source_len.to_be_bytes());
        out.extend_from_slice(&source);
        out.extend_from_slice(&vote_count.to_be_bytes());
        for vote in &self.votes {
            out.extend_from_slice(&vote.validator_id().value().to_be_bytes());
            out.extend_from_slice(&vote.signature_bytes());
        }
        Ok(out)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, ValidatorTransitionSourceCodecError> {
        if bytes.len() > MAX_VALIDATOR_TRANSITION_PROOF_SIZE {
            return Err(ValidatorTransitionSourceCodecError::TooLarge);
        }
        let mut decoder = Decoder::new(bytes);
        let source_len = usize::from(decoder.u16()?);
        if source_len == 0 || source_len > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        let source = ValidatorSetTransitionSource::decode_bytes(decoder.take(source_len)?)?;
        let vote_count = usize::from(decoder.u16()?);
        if vote_count == 0 || vote_count > decoder.remaining() / ENCODED_TRANSITION_VOTE_SIZE {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        let mut votes = Vec::with_capacity(vote_count);
        for _ in 0..vote_count {
            votes.push(ValidatorVote::from_untrusted_parts(
                ValidatorId::new(decoder.u64()?),
                decoder.array_64()?,
            ));
        }
        if !decoder.is_done() {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        Ok(Self { source, votes })
    }
}

impl ValidatorSetTransitionSource {
    pub fn new(
        next_validator_set: ValidatorSet,
        admissions: Vec<ValidatorAdmissionRequest>,
        consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
        currency_frontier: u64,
    ) -> Self {
        Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            currency_frontier,
            next_validator_set,
            admissions,
            consensus_key_rotations,
            handoff_digest: None,
        }
    }

    pub fn from_transition(transition: &ValidatorSetTransition) -> Self {
        let mut source = Self::new(
            transition.next_validator_set().clone(),
            transition
                .admissions()
                .iter()
                .map(|admission| admission.request().clone())
                .collect(),
            transition.consensus_key_rotations().to_vec(),
            transition.currency_frontier(),
        );
        source.handoff_digest = transition.handoff_digest;
        source
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
    pub const fn task_handoff_digest(&self) -> Option<[u8; 32]> {
        self.handoff_digest
    }

    pub fn next_validator_set(&self) -> &ValidatorSet {
        &self.next_validator_set
    }

    pub(crate) fn currency_frontier(&self) -> u64 {
        self.currency_frontier
    }

    pub fn admissions(&self) -> &[ValidatorAdmissionRequest] {
        &self.admissions
    }

    pub fn consensus_key_rotations(&self) -> &[ValidatorConsensusKeyRotationRequest] {
        &self.consensus_key_rotations
    }

    pub fn verify(
        &self,
        current_validator_set: &ValidatorSet,
        validator_registry: &ValidatorRegistry,
    ) -> Result<ValidatorSetTransition, ValidatorTransitionSourceError> {
        let admissions = self
            .admissions
            .iter()
            .map(ValidatorAdmissionRequest::verify)
            .collect::<Result<Vec<_>, _>>()
            .map_err(ValidatorTransitionSourceError::Admission)?;

        ValidatorSetTransition::new(
            self.protocol_version,
            current_validator_set,
            validator_registry,
            self.next_validator_set.clone(),
            admissions,
            self.consensus_key_rotations.clone(),
            self.currency_frontier,
        )
        .map(|transition| transition.with_handoff_digest(self.handoff_digest))
        .map_err(ValidatorTransitionSourceError::Transition)
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>, ValidatorTransitionSourceCodecError> {
        let validator_count = u16::try_from(self.next_validator_set.len())
            .map_err(|_| ValidatorTransitionSourceCodecError::TooLarge)?;
        let admission_count = u16::try_from(self.admissions.len())
            .map_err(|_| ValidatorTransitionSourceCodecError::TooLarge)?;
        let rotation_count = u16::try_from(self.consensus_key_rotations.len())
            .map_err(|_| ValidatorTransitionSourceCodecError::TooLarge)?;

        let mut out = Vec::new();
        out.extend_from_slice(&self.protocol_version.to_be_bytes());
        out.extend_from_slice(&self.currency_frontier.to_be_bytes());
        out.push(u8::from(self.handoff_digest.is_some()));
        if let Some(digest) = self.handoff_digest {
            out.extend_from_slice(&digest);
        }
        out.extend_from_slice(&self.next_validator_set.version().to_be_bytes());
        out.extend_from_slice(&validator_count.to_be_bytes());
        for credential in self.next_validator_set.credentials() {
            encode_credential(&mut out, credential);
        }
        out.extend_from_slice(&admission_count.to_be_bytes());
        for request in &self.admissions {
            encode_admission(&mut out, request);
        }
        out.extend_from_slice(&rotation_count.to_be_bytes());
        for request in &self.consensus_key_rotations {
            encode_rotation(&mut out, request);
        }
        if out.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
            return Err(ValidatorTransitionSourceCodecError::TooLarge);
        }
        Ok(out)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, ValidatorTransitionSourceCodecError> {
        if bytes.len() > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
            return Err(ValidatorTransitionSourceCodecError::TooLarge);
        }
        let mut decoder = Decoder::new(bytes);
        let protocol_version = decoder.u32()?;
        let currency_frontier = decoder.u64()?;
        let handoff_digest = match decoder.take(1)?[0] {
            0 => None,
            1 => Some(decoder.array_32()?),
            _ => return Err(ValidatorTransitionSourceCodecError::InvalidLength),
        };
        let next_version = decoder.u64()?;
        let validator_count = usize::from(decoder.u16()?);
        let maximum_credentials = MAX_VALIDATOR_TRANSITION_SOURCE_SIZE / CREDENTIAL_SIZE;
        if validator_count == 0 || validator_count > maximum_credentials {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        let mut credentials = Vec::with_capacity(validator_count);
        for _ in 0..validator_count {
            credentials.push(decode_credential(&mut decoder)?);
        }
        let next_validator_set = ValidatorSet::new(next_version, credentials)
            .map_err(ValidatorTransitionSourceCodecError::InvalidValidatorSet)?;

        let admission_count = usize::from(decoder.u16()?);
        if admission_count > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE / ADMISSION_SIZE {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        let mut admissions = Vec::with_capacity(admission_count);
        for _ in 0..admission_count {
            admissions.push(decode_admission(&mut decoder)?);
        }

        let rotation_count = usize::from(decoder.u16()?);
        if rotation_count > MAX_VALIDATOR_TRANSITION_SOURCE_SIZE / ROTATION_SIZE {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }
        let mut rotations = Vec::with_capacity(rotation_count);
        for _ in 0..rotation_count {
            rotations.push(decode_rotation(&mut decoder)?);
        }
        if !decoder.is_done() {
            return Err(ValidatorTransitionSourceCodecError::InvalidLength);
        }

        Ok(Self {
            protocol_version,
            currency_frontier,
            next_validator_set,
            admissions,
            consensus_key_rotations: rotations,
            handoff_digest,
        })
    }
}

fn encode_credential(out: &mut Vec<u8>, credential: &ValidatorCredential) {
    out.extend_from_slice(&credential.id().value().to_be_bytes());
    out.extend_from_slice(&credential.identity_public_key());
    out.extend_from_slice(&credential.consensus_public_key());
    out.extend_from_slice(&credential.recovery_public_key());
}

fn decode_credential(
    decoder: &mut Decoder<'_>,
) -> Result<ValidatorCredential, ValidatorTransitionSourceCodecError> {
    let validator_id = ValidatorId::new(decoder.u64()?);
    ValidatorCredential::new(
        validator_id,
        decoder.array_32()?,
        decoder.array_32()?,
        decoder.array_32()?,
    )
    .map_err(ValidatorTransitionSourceCodecError::InvalidValidatorSet)
}

fn encode_admission(out: &mut Vec<u8>, request: &ValidatorAdmissionRequest) {
    out.extend_from_slice(&request.protocol_version().to_be_bytes());
    encode_credential(out, request.credential());
    out.extend_from_slice(&request.identity_signature());
    out.extend_from_slice(&request.consensus_signature());
    out.extend_from_slice(&request.recovery_signature());
}

fn decode_admission(
    decoder: &mut Decoder<'_>,
) -> Result<ValidatorAdmissionRequest, ValidatorTransitionSourceCodecError> {
    let protocol_version = decoder.u32()?;
    let credential = decode_credential(decoder)?;
    Ok(ValidatorAdmissionRequest::from_untrusted_parts(
        protocol_version,
        credential,
        decoder.array_64()?,
        decoder.array_64()?,
        decoder.array_64()?,
    ))
}

fn encode_rotation(out: &mut Vec<u8>, request: &ValidatorConsensusKeyRotationRequest) {
    out.extend_from_slice(&request.protocol_version().to_be_bytes());
    out.push(request.authority().tag());
    out.extend_from_slice(&request.validator_id().value().to_be_bytes());
    out.extend_from_slice(&request.current_validator_set_version().to_be_bytes());
    out.extend_from_slice(&request.new_consensus_public_key());
    out.extend_from_slice(&request.signature_bytes());
}

fn decode_rotation(
    decoder: &mut Decoder<'_>,
) -> Result<ValidatorConsensusKeyRotationRequest, ValidatorTransitionSourceCodecError> {
    let protocol_version = decoder.u32()?;
    let authority = ValidatorRotationAuthority::from_tag(decoder.u8()?)
        .ok_or(ValidatorTransitionSourceCodecError::InvalidRotationAuthority)?;
    Ok(ValidatorConsensusKeyRotationRequest::from_untrusted_parts(
        protocol_version,
        authority,
        ValidatorId::new(decoder.u64()?),
        decoder.u64()?,
        decoder.array_32()?,
        decoder.array_64()?,
    ))
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ValidatorTransitionSourceCodecError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ValidatorTransitionSourceCodecError::InvalidLength)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(ValidatorTransitionSourceCodecError::InvalidLength)?;
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ValidatorTransitionSourceCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ValidatorTransitionSourceCodecError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(
            |_| ValidatorTransitionSourceCodecError::InvalidLength,
        )?))
    }

    fn u32(&mut self) -> Result<u32, ValidatorTransitionSourceCodecError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(
            |_| ValidatorTransitionSourceCodecError::InvalidLength,
        )?))
    }

    fn u64(&mut self) -> Result<u64, ValidatorTransitionSourceCodecError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_| ValidatorTransitionSourceCodecError::InvalidLength,
        )?))
    }

    fn array_32(&mut self) -> Result<[u8; 32], ValidatorTransitionSourceCodecError> {
        self.take(32)?
            .try_into()
            .map_err(|_| ValidatorTransitionSourceCodecError::InvalidLength)
    }

    fn array_64(&mut self) -> Result<[u8; 64], ValidatorTransitionSourceCodecError> {
        self.take(64)?
            .try_into()
            .map_err(|_| ValidatorTransitionSourceCodecError::InvalidLength)
    }

    const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    const fn is_done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
