//! Native Contract::validate and the authorization steps in Node::validate /
//! Node::execute. Call individual steps at their native position: eagerly
//! checking every signature would change read costs and failure ordering.
use crate::{input, Hash, Record, Value};
use mmx_crypto::{public_key_address, verify, VerifyError};
use std::collections::BTreeSet;

pub type AuthResult<T> = std::result::Result<T, AuthError>;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidSolution {
    #[error("missing")]
    Missing,
    #[error("missing solution")]
    MissingInputSolution,
    #[error("invalid type")]
    InvalidType,
    #[error("wrong public key")]
    WrongPublicKey { expected: Hash, actual: Hash },
    #[error("invalid signature")]
    InvalidSignature { owner: Option<Hash> },
    #[error("num_required != 1")]
    ThresholdNotOne,
    #[error("no such owner")]
    NoSuchOwner(Hash),
    #[error("wrong num_required: {actual} != {expected}")]
    WrongThreshold { expected: u32, actual: u32 },
    #[error("invalid signature type")]
    InvalidSignatureType(Hash),
    #[error("insufficient signatures: {count} < {required}")]
    InsufficientSignatures { required: u32, count: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing transaction sender")]
    MissingSender,
    #[error("missing sender signature")]
    MissingSenderSignature,
    #[error("invalid solution ({0})")]
    InvalidSolution(#[from] InvalidSolution),
    #[error(transparent)]
    Crypto(#[from] VerifyError),
    #[error("no such contract")]
    NoSuchContract(Hash),
    #[error("invalid operation")]
    InvalidOperation,
    #[error(transparent)]
    Input(#[from] crate::Error),
}
impl AuthError {
    /// Native invalid_solution derives from static_failure. Crypto parsing,
    /// missing contracts and unsupported contract validation are ordinary
    /// execution failures. Sender authorization occurs outside the dynamic
    /// failure handler, so *every* sender error rejects the transaction.
    pub fn is_static_failure(&self) -> bool {
        matches!(
            self,
            Self::MissingSender | Self::MissingSenderSignature | Self::InvalidSolution(_)
        )
    }
}

fn bytes<const N: usize>(value: &Value) -> AuthResult<[u8; N]> {
    value
        .bytes()?
        .try_into()
        .map_err(|_| input(format!("expected {N} bytes")).into())
}
fn uint32(record: &Record, field: &str) -> AuthResult<u32> {
    record
        .uint(field)?
        .try_into()
        .map_err(|_| input("expected uint32").into())
}
fn pubkey(solution: &Record) -> AuthResult<[u8; 33]> {
    bytes(solution.field("pubkey")?)
}
fn check_signature(solution: &Record, txid: &Hash, owner: Option<Hash>) -> AuthResult<()> {
    let key = pubkey(solution)?;
    let signature = bytes(solution.field("signature")?)?;
    if !verify(txid, &key, &signature)? {
        return Err(InvalidSolution::InvalidSignature { owner }.into());
    }
    Ok(())
}
fn public_key_solution(address: &Hash, solution: Option<&Record>, txid: &Hash) -> AuthResult<()> {
    let solution = solution.ok_or(InvalidSolution::Missing)?;
    if solution.name != "mmx.solution.PubKey" {
        return Err(InvalidSolution::InvalidType.into());
    }
    let actual = public_key_address(&pubkey(solution)?);
    if actual != *address {
        return Err(InvalidSolution::WrongPublicKey {
            expected: *address,
            actual,
        }
        .into());
    }
    check_signature(solution, txid, Some(*address))
}

/// Authenticate a native PubKey or MultiSig contract. Other contracts inherit
/// Contract::validate's "invalid operation" failure. This deliberately does
/// not run static is_valid: native callers perform that separately, and version
/// 0 multisig validation can skip invalid or unrelated nested solutions.
pub fn validate_solution(
    contract: &Record,
    solution: Option<&Record>,
    txid: &Hash,
    tx_version: u32,
) -> AuthResult<()> {
    if contract.name == "mmx.contract.PubKey" {
        return public_key_solution(&bytes(contract.field("address")?)?, solution, txid);
    }
    if contract.name != "mmx.contract.MultiSig" {
        return Err(AuthError::InvalidOperation);
    }
    let required = uint32(contract, "num_required")?;
    let solution = solution.ok_or(InvalidSolution::Missing)?;
    if solution.name == "mmx.solution.PubKey" {
        if required != 1 {
            return Err(InvalidSolution::ThresholdNotOne.into());
        }
        let owner = public_key_address(&pubkey(solution)?);
        let owners = owners(contract)?;
        if !owners.contains(&owner) {
            return Err(InvalidSolution::NoSuchOwner(owner).into());
        }
        return check_signature(solution, txid, None);
    }
    if solution.name != "mmx.solution.MultiSig" {
        return Err(InvalidSolution::InvalidType.into());
    }
    if tx_version >= 1 {
        let actual = uint32(solution, "num_required")?;
        if actual != required {
            return Err(InvalidSolution::WrongThreshold {
                expected: required,
                actual,
            }
            .into());
        }
    }
    let owners = owners(contract)?;
    let mut count = 0;
    for (owner, value) in solution.map("solutions")? {
        let owner = bytes(owner)?;
        if !owners.contains(&owner) {
            if tx_version >= 1 {
                return Err(InvalidSolution::NoSuchOwner(owner).into());
            }
            continue;
        }
        let solution = value.optional()?.map(Value::record).transpose()?;
        let solution = match solution {
            Some(value) if value.name == "mmx.solution.PubKey" => value,
            _ if tx_version == 0 => continue,
            _ => return Err(InvalidSolution::InvalidSignatureType(owner).into()),
        };
        let actual = public_key_address(&pubkey(solution)?);
        if actual != owner {
            return Err(InvalidSolution::WrongPublicKey {
                expected: owner,
                actual,
            }
            .into());
        }
        check_signature(solution, txid, Some(owner))?;
        count += 1;
    }
    if count < required as usize {
        return Err(InvalidSolution::InsufficientSignatures { required, count }.into());
    }
    Ok(())
}
fn owners(contract: &Record) -> AuthResult<BTreeSet<Hash>> {
    contract.seq("owners")?.iter().map(bytes).collect()
}
fn transaction(record: &Record) -> AuthResult<(Hash, u32)> {
    if record.name != "mmx.Transaction" {
        return Err(input("expected transaction").into());
    }
    Ok((bytes(record.field("id")?)?, uint32(record, "version")?))
}
fn solution_at(transaction: &Record, index: usize) -> AuthResult<Option<&Record>> {
    transaction
        .seq("solutions")?
        .get(index)
        .map(|v| v.optional()?.map(Value::record).transpose())
        .transpose()
        .map(Option::flatten)
        .map_err(Into::into)
}

/// Sender always uses a PubKey contract at its address and solutions[0]. A
/// deployed multisig contract at that address does not authorize fee payment.
/// Run static transaction validation before calling this function.
pub fn authorize_sender(tx: &Record) -> AuthResult<Hash> {
    let (txid, _) = transaction(tx)?;
    let address = tx
        .field("sender")?
        .optional()?
        .ok_or(AuthError::MissingSender)?;
    let address = bytes(address)?;
    if tx.seq("solutions")?.is_empty() {
        return Err(AuthError::MissingSenderSignature);
    }
    public_key_solution(&address, solution_at(tx, 0)?, &txid)?;
    Ok(address)
}

/// Authorize one explicit input, after the native balance check. For IS_EXEC
/// inputs, lookup must charge get_contract_ex's read cost before returning.
/// Unflagged inputs always authenticate as PubKey without any contract lookup.
pub fn authorize_input(
    tx: &Record,
    index: usize,
    lookup: impl FnOnce(&Hash) -> AuthResult<Option<Record>>,
) -> AuthResult<()> {
    let (txid, version) = transaction(tx)?;
    let input_record = tx
        .seq("inputs")?
        .get(index)
        .ok_or_else(|| input("input index"))?
        .record()?;
    let solution = solution_at(tx, input_record.uint("solution")? as usize)?
        .ok_or(InvalidSolution::MissingInputSolution)?;
    let address = bytes(input_record.field("address")?)?;
    if input_record.uint("flags")? & 1 == 0 {
        return public_key_solution(&address, Some(solution), &txid);
    }
    let contract = lookup(&address)?.ok_or(AuthError::NoSuchContract(address))?;
    validate_solution(&contract, Some(solution), &txid, version)
}

/// Authorize one Execute/Deposit user at Node::execute's setup point. Lookup
/// must charge the user-contract read against that engine's gas meter; absent
/// records fall back to PubKey. Anonymous calls perform no lookup or signature
/// access. The target must already have been checked to be executable.
pub fn authorize_operation_user(
    tx: &Record,
    index: usize,
    lookup: impl FnOnce(&Hash) -> AuthResult<Option<Record>>,
) -> AuthResult<Option<Hash>> {
    let (txid, version) = transaction(tx)?;
    let op = tx
        .seq("execute")?
        .get(index)
        .ok_or_else(|| input("operation index"))?
        .record()?;
    if !matches!(
        op.name.as_str(),
        "mmx.operation.Execute" | "mmx.operation.Deposit"
    ) {
        return Err(AuthError::InvalidOperation);
    }
    let Some(user) = op.field("user")?.optional()? else {
        return Ok(None);
    };
    let user = bytes(user)?;
    let contract = lookup(&user)?;
    let solution = solution_at(tx, op.uint("solution")? as usize)?;
    match contract {
        Some(contract) => validate_solution(&contract, solution, &txid, version)?,
        None => public_key_solution(&user, solution, &txid)?,
    }
    Ok(Some(user))
}
