//! Consensus plot-NFT reward targets from Node::get_plot_nft_target and
//! Node::call_contract. Calls run against the original ledger snapshot, with
//! no transaction host callbacks and no publication of writes or transfers.
use crate::{
    contracts::failure,
    overlay::{Overlay, Shared},
    Contract,
};
use mmx_types::mmx;
use mmx_vm::{
    address, Address, Engine, InputValue, Result, Storage, Value, EXTERN_ADDRESS, EXTERN_HEIGHT,
    EXTERN_NETWORK, EXTERN_TXID, EXTERN_USER, MEM_EXTERN, MEM_STACK,
};
use std::{cell::RefCell, rc::Rc};

fn lookup<S: Storage>(storage: &S, address: &Address, name: &str) -> Result<Option<Contract>> {
    Ok(storage
        .get_contract(address)?
        .map(Contract::decode)
        .transpose()?
        .filter(|c| c.type_name() == name))
}

/// Resolve using the current ledger peak's height (the candidate's parent
/// height during block validation), not the candidate block's height. Missing
/// executables/binaries/methods fall back to `contract`; other errors propagate.
pub fn target<S: Storage>(
    storage: &S,
    params: &mmx::ChainParams,
    height: u32,
    contract: Address,
    farmer: Option<Address>,
) -> Result<Address> {
    // This first lookup precedes the native resolver's try/catch.
    let Some(exec) = lookup(storage, &contract, "mmx.contract.Executable")? else {
        return Ok(contract);
    };
    let resolve = || {
        let executable = exec.executable()?;
        let Some(binary) = lookup(storage, &executable.binary, "mmx.contract.Binary")? else {
            return Ok(contract);
        };
        let binary = binary.binary()?;
        if !binary.methods.contains_key(b"mmx_reward_target".as_slice()) {
            return Ok(contract);
        }
        // Native call_contract repeats both lookups after resolver preflight.
        let exec = lookup(storage, &contract, "mmx.contract.Executable")?
            .ok_or_else(|| failure("no such contract"))?
            .executable()?;
        let binary = lookup(storage, &exec.binary, "mmx.contract.Binary")?
            .ok_or_else(|| failure("no such binary"))?
            .binary()?;
        let method = binary
            .methods
            .get(b"mmx_reward_target".as_slice())
            .ok_or_else(|| failure("no such method: mmx_reward_target"))?;
        let mut snapshot = Shared(Rc::new(RefCell::new(Overlay::new(storage))));
        let mut vm = Engine::new(
            contract,
            &mut snapshot,
            method.is_const,
            u32::from(height >= params.hardfork2_height),
            params.max_tx_cost,
        );
        let code = vm.load_binary_parts(&binary.constant, &binary.binary)?;
        vm.write(MEM_EXTERN + EXTERN_TXID, Value::Nil)?;
        vm.write(MEM_EXTERN + EXTERN_HEIGHT, Value::from(height as u64))?;
        vm.write(
            MEM_EXTERN + EXTERN_ADDRESS,
            Value::Binary(contract.to_vec()),
        )?;
        vm.write(
            MEM_EXTERN + EXTERN_NETWORK,
            Value::String(params.network.as_bytes().to_vec()),
        )?;
        vm.write(MEM_EXTERN + EXTERN_USER, Value::Nil)?;
        let arg = farmer.map_or(InputValue::Nil, |a| {
            InputValue::String(address::encode(&a).into_bytes())
        });
        vm.set_args(&[arg])?;
        // Native call_contract checks neither visibility nor constructor flags.
        // It installs no RCALL/CREAD callbacks and calls execute(..., false).
        vm.run(&code, method.entry_point)?;
        vm.charge_gas(0)?;
        vm.read_address_result(MEM_STACK)
    };
    resolve().map_err(|e| failure(format!("mmx_reward_target() failed with: {e}")))
}

pub(crate) fn validate<S: Storage>(
    storage: &S,
    context: &crate::block::BlockContext,
    block: &mmx_consensus::Record,
) -> std::result::Result<(), crate::transaction::ValidationError> {
    use crate::transaction::{bytes, ValidationError};
    let optional = |field| block.field(field)?.optional()?.map(bytes::<32>).transpose();
    let reward = optional("reward_addr")?;
    let expected = if reward.is_some() {
        let proof = block
            .field("proof")?
            .seq()?
            .first()
            .ok_or_else(|| ValidationError::Dynamic("block has no proof".into()))?
            .record()?;
        if proof.name == "mmx.ProofOfSpaceNFT" {
            Some(bytes(proof.field("contract")?)?)
        } else {
            None
        }
    } else {
        None
    };
    if optional("reward_contract")? != expected {
        return Err(ValidationError::Dynamic("invalid reward_contract".into()));
    }
    if let Some(contract) = expected {
        let expected = target(
            storage,
            &context.params,
            context.previous.height,
            contract,
            optional("reward_account")?,
        )
        .map_err(|e| {
            ValidationError::Dynamic(format!("reward_addr resolution failed with: {e}"))
        })?;
        if reward != Some(expected) {
            return Err(ValidationError::Dynamic(format!(
                "invalid reward_addr for reward_contract: expected {}",
                address::encode(&expected)
            )));
        }
    }
    Ok(())
}
