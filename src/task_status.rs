#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegalTaskStatus {
    Unknown,
    Bound,
    Prepared,
    Voting,
    Finalized,
    Succeeded,
}
