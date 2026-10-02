use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::{
    BftError, BftPhase, BftProposal, BftProposalSubject, BftQuorumCertificate, BftStatement,
    BftValue, BftVote, ConsensusScope, PersistenceError, StateStore, ValidatorId, ValidatorSet,
    ValidatorSigner, ValidatorSigningError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BftDriverPhase {
    Proposal,
    Prevote,
    Precommit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BftTimeoutConfig {
    pub proposal: Duration,
    pub prevote: Duration,
    pub precommit: Duration,
}

impl BftTimeoutConfig {
    pub const fn new(proposal: Duration, prevote: Duration, precommit: Duration) -> Self {
        Self {
            proposal,
            prevote,
            precommit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftDriverAction {
    Noop,
    Vote {
        statement: BftStatement,
        vote: BftVote,
    },
    QuorumCertificate(BftQuorumCertificate),
    FinalityReady {
        round: u64,
        digest: [u8; 32],
    },
    RoundAdvanced {
        round: u64,
        proposer: ValidatorId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BftDriverError {
    Bft(BftError),
    Persistence(PersistenceError),
    Signing(ValidatorSigningError),
    ScopeMismatch,
    SubjectMismatch,
    RoundMismatch { current: u64, actual: u64 },
    RoundOverflow,
}

impl From<BftError> for BftDriverError {
    fn from(value: BftError) -> Self {
        Self::Bft(value)
    }
}

impl From<PersistenceError> for BftDriverError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

impl From<ValidatorSigningError> for BftDriverError {
    fn from(value: ValidatorSigningError) -> Self {
        Self::Signing(value)
    }
}

pub struct BftDriver {
    signer: ValidatorSigner,
    store: StateStore,
    validator_set: ValidatorSet,
    scope: ConsensusScope,
    votes: BTreeMap<(u64, BftPhase, BftValue), BTreeMap<ValidatorId, BftVote>>,
    certified: BTreeSet<(u64, BftPhase, BftValue)>,
    validated_digests: BTreeSet<[u8; 32]>,
}

impl BftDriver {
    pub fn new(
        signer: ValidatorSigner,
        store: StateStore,
        validator_set: ValidatorSet,
        scope: ConsensusScope,
    ) -> Result<Self, BftDriverError> {
        if !scope.matches_validator_set_version(validator_set.version()) {
            return Err(BftDriverError::ScopeMismatch);
        }
        Ok(Self {
            signer,
            store,
            validator_set,
            scope,
            votes: BTreeMap::new(),
            certified: BTreeSet::new(),
            validated_digests: BTreeSet::new(),
        })
    }

    pub fn scope(&self) -> &ConsensusScope {
        &self.scope
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.signer.validator_id()
    }

    pub fn current_round(&self) -> Result<u64, BftDriverError> {
        Ok(self
            .store
            .bft_local_state(self.signer.validator_id(), &self.scope)?
            .map(|state| state.round())
            .unwrap_or(0))
    }

    pub fn phase(&self) -> Result<BftDriverPhase, BftDriverError> {
        let state = self
            .store
            .bft_local_state(self.signer.validator_id(), &self.scope)?;
        Ok(match state {
            Some(state) if state.precommit().is_some() => BftDriverPhase::Precommit,
            Some(state) if state.prevote().is_some() => BftDriverPhase::Prevote,
            _ => BftDriverPhase::Proposal,
        })
    }

    pub fn proposer(&self) -> Result<ValidatorId, BftDriverError> {
        Ok(self.validator_set.proposer(self.current_round()?))
    }

    pub fn register_subject(&mut self, subject: &BftProposalSubject) -> Result<(), BftDriverError> {
        self.validate_subject(subject)?;
        self.validated_digests.insert(subject.digest());
        Ok(())
    }

    pub fn create_proposal(
        &mut self,
        subject: &BftProposalSubject,
    ) -> Result<BftProposal, BftDriverError> {
        self.validate_subject(subject)?;
        let round = self.current_round()?;
        let proposal = self
            .signer
            .sign_bft_proposal(subject, round, &self.validator_set)?;
        self.validated_digests.insert(subject.digest());
        Ok(proposal)
    }

    pub fn accept_proposal(
        &mut self,
        proposal: &BftProposal,
        subject: &BftProposalSubject,
        unlock_certificate: Option<&BftQuorumCertificate>,
    ) -> Result<BftDriverAction, BftDriverError> {
        self.validate_subject(subject)?;
        proposal.verify(&self.validator_set)?;
        if proposal.scope() != &self.scope {
            return Err(BftDriverError::ScopeMismatch);
        }
        let current = self.current_round()?;
        if proposal.round() != current {
            return Err(BftDriverError::RoundMismatch {
                current,
                actual: proposal.round(),
            });
        }
        if !proposal.matches_subject(subject) {
            return Err(BftDriverError::SubjectMismatch);
        }

        let value = BftValue::Digest(proposal.subject_digest());
        let vote = self.signer.sign_bft_prevote(
            self.scope.clone(),
            current,
            value,
            &self.validator_set,
            unlock_certificate,
        )?;
        self.validated_digests.insert(proposal.subject_digest());
        Ok(BftDriverAction::Vote {
            statement: BftStatement::new(
                self.validator_set.version(),
                self.scope.clone(),
                current,
                BftPhase::Prevote,
                value,
            ),
            vote,
        })
    }

    pub fn ingest_vote(
        &mut self,
        statement: BftStatement,
        vote: BftVote,
    ) -> Result<Option<BftDriverAction>, BftDriverError> {
        if statement.scope() != &self.scope {
            return Err(BftDriverError::ScopeMismatch);
        }
        let current = self.current_round()?;
        if statement.round() != current
            && !(statement.round() < current
                && matches!(
                    (statement.phase(), statement.value()),
                    (BftPhase::Precommit, BftValue::Digest(_))
                ))
        {
            return Err(BftDriverError::RoundMismatch {
                current,
                actual: statement.round(),
            });
        }
        vote.verify(&statement, &self.validator_set)?;

        let key = (statement.round(), statement.phase(), statement.value());
        let votes = self.votes.entry(key).or_default();
        votes.entry(vote.validator_id()).or_insert(vote);
        if votes.len() < self.validator_set.quorum_threshold() {
            return Ok(None);
        }

        if !self.certified.insert(key) {
            return Ok(None);
        }
        let certificate = BftQuorumCertificate::new(
            statement,
            votes.values().cloned().collect(),
            &self.validator_set,
        )?;
        Ok(Some(BftDriverAction::QuorumCertificate(certificate)))
    }

    pub fn accept_quorum_certificate(
        &mut self,
        certificate: &BftQuorumCertificate,
    ) -> Result<BftDriverAction, BftDriverError> {
        certificate.verify(&self.validator_set)?;
        let statement = certificate.statement();
        if statement.scope() != &self.scope {
            return Err(BftDriverError::ScopeMismatch);
        }
        if let (BftPhase::Precommit, BftValue::Digest(digest)) =
            (statement.phase(), statement.value())
        {
            self.require_validated_subject(digest)?;
            self.store.accept_bft_precommit_qc(
                self.signer.validator_id(),
                certificate,
                &self.validator_set,
            )?;
            return Ok(BftDriverAction::FinalityReady {
                round: statement.round(),
                digest,
            });
        }

        if let BftValue::Digest(digest) = statement.value() {
            self.require_validated_subject(digest)?;
        }

        let mut current = self.current_round()?;
        if statement.round() > current {
            self.store.catch_up_bft_round(
                self.signer.validator_id(),
                &self.scope,
                statement.round(),
                &self.validator_set,
            )?;
            self.prune_after_round_advance(statement.round());
            current = statement.round();
        }
        if statement.round() != current {
            return Err(BftDriverError::RoundMismatch {
                current,
                actual: statement.round(),
            });
        }

        match (statement.phase(), statement.value()) {
            (BftPhase::Prevote, value) => {
                let local_state = self
                    .store
                    .bft_local_state(self.signer.validator_id(), &self.scope)?;
                if local_state.is_some_and(|state| state.precommit().is_some()) {
                    return Ok(BftDriverAction::Noop);
                }
                let prevote_certificate =
                    matches!(value, BftValue::Digest(_)).then_some(certificate);
                let vote = self.signer.sign_bft_precommit(
                    self.scope.clone(),
                    current,
                    value,
                    &self.validator_set,
                    prevote_certificate,
                )?;
                Ok(BftDriverAction::Vote {
                    statement: BftStatement::new(
                        self.validator_set.version(),
                        self.scope.clone(),
                        current,
                        BftPhase::Precommit,
                        value,
                    ),
                    vote,
                })
            }
            (BftPhase::Precommit, BftValue::Nil) => {
                let next = current
                    .checked_add(1)
                    .ok_or(BftDriverError::RoundOverflow)?;
                self.store.accept_bft_nil_precommit_qc(
                    self.signer.validator_id(),
                    certificate,
                    &self.validator_set,
                )?;
                self.prune_after_round_advance(next);
                Ok(BftDriverAction::RoundAdvanced {
                    round: next,
                    proposer: self.validator_set.proposer(next),
                })
            }
            (BftPhase::Precommit, BftValue::Digest(_)) => {
                unreachable!("digest precommit QCs are handled before round checks")
            }
        }
    }

    pub fn on_timeout(&mut self) -> Result<BftDriverAction, BftDriverError> {
        let round = self.current_round()?;
        match self.phase()? {
            BftDriverPhase::Proposal => {
                let vote = self.signer.sign_bft_prevote(
                    self.scope.clone(),
                    round,
                    BftValue::Nil,
                    &self.validator_set,
                    None,
                )?;
                Ok(BftDriverAction::Vote {
                    statement: BftStatement::new(
                        self.validator_set.version(),
                        self.scope.clone(),
                        round,
                        BftPhase::Prevote,
                        BftValue::Nil,
                    ),
                    vote,
                })
            }
            BftDriverPhase::Prevote => {
                let vote = self.signer.sign_bft_precommit(
                    self.scope.clone(),
                    round,
                    BftValue::Nil,
                    &self.validator_set,
                    None,
                )?;
                Ok(BftDriverAction::Vote {
                    statement: BftStatement::new(
                        self.validator_set.version(),
                        self.scope.clone(),
                        round,
                        BftPhase::Precommit,
                        BftValue::Nil,
                    ),
                    vote,
                })
            }
            BftDriverPhase::Precommit => self.advance_round(),
        }
    }

    pub async fn wait_for_timeout(
        &mut self,
        config: BftTimeoutConfig,
    ) -> Result<BftDriverAction, BftDriverError> {
        let delay = match self.phase()? {
            BftDriverPhase::Proposal => config.proposal,
            BftDriverPhase::Prevote => config.prevote,
            BftDriverPhase::Precommit => config.precommit,
        };
        tokio::time::sleep(delay).await;
        self.on_timeout()
    }

    fn advance_round(&mut self) -> Result<BftDriverAction, BftDriverError> {
        let current = self.current_round()?;
        let next = current
            .checked_add(1)
            .ok_or(BftDriverError::RoundOverflow)?;
        self.store
            .advance_bft_round(self.signer.validator_id(), &self.scope, next)?;
        self.prune_after_round_advance(next);
        Ok(BftDriverAction::RoundAdvanced {
            round: next,
            proposer: self.validator_set.proposer(next),
        })
    }

    fn prune_after_round_advance(&mut self, current: u64) {
        self.votes.retain(|(round, phase, value), _| {
            *round >= current
                || (*phase == BftPhase::Precommit && matches!(value, BftValue::Digest(_)))
        });
        self.certified.retain(|(round, _, _)| *round >= current);
    }

    fn require_validated_subject(&self, digest: [u8; 32]) -> Result<(), BftDriverError> {
        if self.validated_digests.contains(&digest) {
            Ok(())
        } else {
            Err(BftDriverError::SubjectMismatch)
        }
    }

    fn validate_subject(&self, subject: &BftProposalSubject) -> Result<(), BftDriverError> {
        if subject.scope() != &self.scope {
            return Err(BftDriverError::ScopeMismatch);
        }
        if subject.validator_set_version() != self.validator_set.version() {
            return Err(BftDriverError::SubjectMismatch);
        }
        Ok(())
    }
}
