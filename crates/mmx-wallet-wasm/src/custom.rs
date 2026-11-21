//! Custom transactions use the same lossless codec, completion and consensus
//! authorization as native wallets. No private material is returned.
use crate::api::{self, Code, Res, VaultCore};
use mmx_consensus::{authorization, Record};
use mmx_types::{mmx, VnxType};
use mmx_wallet::{public_json, Address};
use serde_json::{json, Value};

const MAX_BYTES: usize = 65_536;
fn invalid<T>(_: T) -> Code {
    api::INVALID_ARGUMENT
}
fn bounded(v: &Value, depth: usize, left: &mut usize) -> Res<()> {
    if depth > 32 || *left == 0 {
        return Err(api::INVALID_ARGUMENT);
    }
    *left -= 1;
    match v {
        Value::Array(a) => {
            for v in a {
                bounded(v, depth + 1, left)?;
            }
        }
        Value::Object(o) => {
            for v in o.values() {
                bounded(v, depth + 1, left)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn parse(text: &str) -> Res<Value> {
    if text.len() > MAX_BYTES {
        return Err(api::INVALID_ARGUMENT);
    }
    let v = serde_json::from_str(text).map_err(invalid)?;
    bounded(&v, 0, &mut 4096)?;
    Ok(v)
}
pub fn params(text: &str) -> Res<mmx::ChainParams> {
    let mut value: Value = serde_json::from_str(text).map_err(invalid)?;
    let obj = value.as_object_mut().ok_or(api::INVALID_ARGUMENT)?;
    // Public chain responses may add presentation-only fields.
    obj.retain(|k, _| {
        k == "__type" || mmx_types::presentation_field("mmx.ChainParams", k).is_some()
    });
    obj.insert("__type".into(), json!("mmx.ChainParams"));
    let record = public_json::decode(&value, "mmx.ChainParams").map_err(invalid)?;
    let params = mmx::ChainParams::decode(&record.encode().map_err(invalid)?).map_err(invalid)?;
    if params.network.is_empty() || !(0..=18).contains(&params.decimals) {
        return Err(api::INVALID_ARGUMENT);
    }
    Ok(params)
}
pub fn template(text: &str) -> Res<Record> {
    let value = parse(text)?;
    let fields = value.as_object().ok_or(api::INVALID_ARGUMENT)?;
    if fields.get("__type").is_some_and(|v| v != "mmx.Transaction") {
        return Err(api::INVALID_ARGUMENT);
    }
    let defaults = Record::from_generated(&mmx::Transaction::default()).map_err(invalid)?;
    let mut full = public_json::encode(&defaults).map_err(invalid)?;
    full.as_object_mut().unwrap().extend(fields.clone());
    let tx = public_json::decode(&full, "mmx.Transaction").map_err(invalid)?;
    if tx
        .field("exec_result")
        .map_err(invalid)?
        .optional()
        .map_err(invalid)?
        .is_some()
        || !tx
            .field("solutions")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
            .is_empty()
        || tx
            .field("inputs")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
            .len()
            > 100
        || tx
            .field("outputs")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
            .len()
            > 100
        || tx
            .field("execute")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
            .len()
            > 32
    {
        return Err(api::INVALID_ARGUMENT);
    }
    Ok(tx)
}
pub fn inspect(text: &str) -> Res<String> {
    serde_json::to_string(&public_json::encode(&template(text)?).map_err(invalid)?).map_err(invalid)
}
fn verify(tx: &Record, p: &mmx::ChainParams) -> Res<()> {
    if !tx.is_valid(&p.into()).map_err(invalid)? {
        return Err(api::INVALID_ARGUMENT);
    }
    authorization::authorize_sender(tx).map_err(invalid)?;
    for (i, input) in tx
        .field("inputs")
        .map_err(invalid)?
        .seq()
        .map_err(invalid)?
        .iter()
        .enumerate()
    {
        // This API funds ordinary wallet accounts; contract/multisig inputs
        // require contract lookups and belong to the separate partial-sign API.
        if input
            .record()
            .map_err(invalid)?
            .field("flags")
            .map_err(invalid)?
            .uint()
            .map_err(invalid)?
            != 0
        {
            return Err(api::INVALID_ARGUMENT);
        }
        authorization::authorize_input(tx, i, |_| Ok(None)).map_err(invalid)?;
    }
    for (i, op) in tx
        .field("execute")
        .map_err(invalid)?
        .seq()
        .map_err(invalid)?
        .iter()
        .enumerate()
    {
        if matches!(
            op.record().map_err(invalid)?.name.as_str(),
            "mmx.operation.Execute" | "mmx.operation.Deposit"
        ) {
            authorization::authorize_operation_user(tx, i, |_| Ok(None)).map_err(invalid)?;
        }
    }
    Ok(())
}
pub fn verify_json(text: &str, params_json: &str) -> Res<()> {
    let value = parse(text)?;
    let tx = public_json::decode(&value, "mmx.Transaction").map_err(invalid)?;
    verify(&tx, &params(params_json)?)
}
impl VaultCore {
    pub fn complete_custom(
        &self,
        text: &str,
        balances: &str,
        chain: &str,
        height: u32,
        fee_level: u32,
        gas_limit: u32,
    ) -> Res<String> {
        let tx = template(text)?;
        let p = params(chain)?;
        if tx
            .field("network")
            .map_err(invalid)?
            .text()
            .map_err(invalid)?
            != b""
            && tx
                .field("network")
                .map_err(invalid)?
                .text()
                .map_err(invalid)?
                != p.network.as_bytes()
        {
            return Err(api::INVALID_ARGUMENT);
        }
        for input in tx
            .field("inputs")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
        {
            let input = input.record().map_err(invalid)?;
            let owner = Address(
                input
                    .field("address")
                    .map_err(invalid)?
                    .bytes()
                    .map_err(invalid)?
                    .try_into()
                    .map_err(invalid)?,
            );
            if !self.wallet.addresses.contains(&owner) {
                return Err(api::INVALID_ARGUMENT);
            }
        }
        let sender = tx
            .field("sender")
            .map_err(invalid)?
            .optional()
            .map_err(invalid)?
            .map(|v| v.bytes().map(|b| b.to_vec()))
            .transpose()
            .map_err(invalid)?
            .map(|v| <[u8; 32]>::try_from(v).map(mmx::addr_t::from))
            .transpose()
            .map_err(invalid)?;
        let options = mmx::spend_options_t {
            sender,
            fee_ratio: api::fee_ratio(fee_level)?,
            gas_limit,
            expire_delta: Some(100),
            ..Default::default()
        };
        let balances = api::parse_balances(balances)?;
        let signed = self
            .wallet
            .complete_native(&tx, &balances, &p, height, &options)
            .map_err(api::wallet_error)?;
        if signed
            .field("expires")
            .map_err(invalid)?
            .uint()
            .map_err(invalid)?
            <= height.into()
        {
            return Err(api::INVALID_ARGUMENT);
        }
        verify(&signed, &p)?;
        // Reserve the approved maximum, including possible execution costs.
        let payer = signed
            .field("sender")
            .map_err(invalid)?
            .optional()
            .map_err(invalid)?
            .ok_or(api::INVALID_ARGUMENT)?;
        let payer = Address(
            payer
                .bytes()
                .map_err(invalid)?
                .try_into()
                .map_err(invalid)?,
        );
        let mut left = balances
            .get(&(payer, Address::default()))
            .copied()
            .unwrap_or(0);
        for input in signed
            .field("inputs")
            .map_err(invalid)?
            .seq()
            .map_err(invalid)?
        {
            let input = input.record().map_err(invalid)?;
            if input
                .field("address")
                .map_err(invalid)?
                .bytes()
                .map_err(invalid)?
                == payer.0
                && input
                    .field("contract")
                    .map_err(invalid)?
                    .bytes()
                    .map_err(invalid)?
                    == [0; 32]
            {
                let amount = u128::from_le_bytes(
                    input
                        .field("amount")
                        .map_err(invalid)?
                        .bytes()
                        .map_err(invalid)?
                        .try_into()
                        .map_err(invalid)?,
                );
                left = left.checked_sub(amount).ok_or(api::INSUFFICIENT_FUNDS)?;
            }
        }
        if left
            < signed
                .field("max_fee_amount")
                .map_err(invalid)?
                .uint()
                .map_err(invalid)?
                .into()
        {
            return Err(api::INSUFFICIENT_FEE);
        }
        serde_json::to_string(&public_json::encode(&signed).map_err(invalid)?).map_err(invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmx_wallet::seed_to_words;
    fn wallet() -> VaultCore {
        VaultCore::open(&seed_to_words(&[42; 32]), None, None, 3).unwrap()
    }
    fn chain() -> String {
        let p = mmx::ChainParams {
            network: "wallet-connect-test".into(),
            hardfork2_height: 0,
            ..Default::default()
        };
        public_json::encode(&Record::from_generated(&p).unwrap())
            .unwrap()
            .to_string()
    }
    #[test]
    fn completes_contract_deposit_with_exact_variant_arguments_and_valid_signatures() {
        let w = wallet();
        let owner = &w.addresses()[0];
        let target = Address([7; 32]).to_string();
        let native = Address::default().to_string();
        let args =
            json!(["12345678901234567890", {"address":"literal","amount":"literal"}, -12, 15]);
        let tx = json!({"__type":"mmx.Transaction", "execute":[{
            "__type":"mmx.operation.Deposit", "address":target, "currency":native,
            "amount":"1000", "method":"deposit", "user":owner, "args":args
        }]});
        let balances =
            json!([{"address":owner,"contract":native,"amount":"100000000"}]).to_string();
        let result = w
            .complete_custom(
                &tx.to_string(),
                &balances,
                &chain(),
                5_050_000,
                1,
                1_000_000,
            )
            .unwrap();
        verify_json(&result, &chain()).unwrap();
        let signed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(signed["execute"][0]["args"], args);
        assert_eq!(signed["execute"][0]["amount"], "1000");
        assert_eq!(signed["sender"], *owner);
        let mut changed = signed;
        changed["execute"][0]["method"] = json!("withdraw");
        assert!(verify_json(&changed.to_string(), &chain()).is_err());
    }
    #[test]
    fn rejects_unknown_fields_receipts_signatures_and_excessive_payloads() {
        for bad in [
            json!({"outputs":[],"surprise":1}),
            json!({"__type":"mmx.Block"}),
            json!({"exec_result":{"__type":"mmx.exec_result_t"}}),
            json!({"solutions":[{"__type":"mmx.solution.PubKey"}]}),
        ] {
            assert!(inspect(&bad.to_string()).is_err(), "{bad}");
        }
        assert!(inspect(&" ".repeat(MAX_BYTES + 1)).is_err());
    }
    #[test]
    fn refuses_foreign_inputs_wrong_network_and_unfunded_maximum_fee() {
        let w = wallet();
        let owner = &w.addresses()[0];
        let native = Address::default().to_string();
        let target = Address([7; 32]).to_string();
        let balances = json!([{"address":owner,"contract":native,"amount":"1000000"}]).to_string();
        for tx in [
            json!({"network":"different"}),
            json!({"inputs":[{"__type":"mmx.txin_t","address":target,"contract":native,"amount":"1"}]}),
            json!({"inputs":[{"__type":"mmx.txin_t","address":owner,"contract":native,"amount":"1","flags":1}]}),
            json!({"expires":1}),
        ] {
            assert!(
                w.complete_custom(&tx.to_string(), &balances, &chain(), 100, 1, 0)
                    .is_err(),
                "{tx}"
            );
        }
        let tx = json!({"outputs":[{"__type":"mmx.txout_t","address":target,"contract":native,"amount":"900000"}]});
        assert_eq!(
            w.complete_custom(&tx.to_string(), &balances, &chain(), 100, 1, 1_000_000),
            Err(api::INSUFFICIENT_FEE)
        );
        assert!(w
            .complete_custom(&tx.to_string(), &balances, &chain(), 100, 1, 0)
            .is_ok());
        assert!(w
            .complete_custom("{}", &balances, &chain(), 100, 4, 0)
            .is_err());
    }
    #[test]
    fn rejects_variant_encodings_that_cannot_round_trip_losslessly() {
        let w = wallet();
        let owner = &w.addresses()[0];
        let native = Address::default().to_string();
        let balances = json!([{"address":owner,"contract":native,"amount":"10000000"}]).to_string();
        for args in [json!([1.5]), json!([{"__type":"not.a.record","value":1}])] {
            let tx = json!({"execute":[{"__type":"mmx.operation.Execute","address":Address([7;32]).to_string(),"method":"call","args":args}]});
            assert!(w
                .complete_custom(&tx.to_string(), &balances, &chain(), 100, 1, 0)
                .is_err());
        }
    }
}
