use crate::{
    CURRENT_PROTOCOL_VERSION, ValidatorAdmissionError, ValidatorAdmissionRequest,
    ValidatorConsensusKeyRotationRequest, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorRotationAuthority, ValidatorSet, ValidatorSetError, ValidatorSetTransition,
    ValidatorTransitionError,
};

pub const MAX_VALIDATOR_TRANSITION_SOURCE_SIZE: usize = 48 * 1024;
const CREDENTIAL_SIZE: usize = 8 + 32 * 3;
const ADMISSION_SIZE: usize = 4 + CREDENTIAL_SIZE + 64 * 3;
const ROTATION_SIZE: usize = 4 + 1 + 8 + 8 + 32 + 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSetTransitionSource {
    protocol_version: u32,
    next_validator_set: ValidatorSet,
    admissions: Vec<ValidatorAdmissionRequest>,
    consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
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

impl ValidatorSetTransitionSource {
    pub fn new(
        next_validator_set: ValidatorSet,
        admissions: Vec<ValidatorAdmissionRequest>,
        consensus_key_rotations: Vec<ValidatorConsensusKeyRotationRequest>,
    ) -> Self {
        Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            next_validator_set,
            admissions,
            consensus_key_rotations,
        }
    }

    pub fn from_transition(transition: &ValidatorSetTransition) -> Self {
        Self::new(
            transition.next_validator_set().clone(),
            transition
                .admissions()
                .iter()
                .map(|admission| admission.request().clone())
                .collect(),
            transition.consensus_key_rotations().to_vec(),
        )
    }

    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn next_validator_set(&self) -> &ValidatorSet {
        &self.next_validator_set
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
        )
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
            next_validator_set,
            admissions,
            consensus_key_rotations: rotations,
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

    const fn is_done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
