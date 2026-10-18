//! Bridges signed wallet transfers into the node's lossless consensus records.
use crate::{Address, ChainParams, Error, Result, Transaction};
use mmx_consensus::{Record, Value};
use mmx_types::mmx;

fn io(address: Address, contract: Address, amount: u128, memo: &Option<String>) -> mmx::txout_t {
    mmx::txout_t {
        address: address.0.into(),
        contract: contract.0.into(),
        amount: amount.to_le_bytes().into(),
        memo: memo.clone(),
    }
}
impl Transaction {
    /// Use native declarations rather than sending wallet presentation JSON to
    /// the VNX transport (addresses, amounts and signatures have different forms).
    pub fn to_record(&self) -> Result<Record> {
        let convert = || -> mmx_consensus::Result<Record> {
            let mut out = Record::from_generated(&mmx::Transaction::default())?;
            for (name, value) in [
                ("id", Value::Bytes(self.id.to_vec())),
                ("version", Value::Uint(self.version.into())),
                ("expires", Value::Uint(self.expires.into())),
                ("fee_ratio", Value::Uint(self.fee_ratio.into())),
                ("static_cost", Value::Uint(self.static_cost.into())),
                ("max_fee_amount", Value::Uint(self.max_fee_amount.into())),
                ("note", Value::Uint(858544509)),
                ("nonce", Value::Uint(self.nonce)),
                ("network", Value::String(self.network.as_bytes().to_vec())),
                (
                    "sender",
                    Value::Optional(self.sender.map(|a| Box::new(Value::Bytes(a.0.to_vec())))),
                ),
                ("content_hash", Value::Bytes(self.content_hash.to_vec())),
            ] {
                out.fields.insert(name.into(), value);
            }
            let inputs = self
                .inputs
                .iter()
                .map(|i| {
                    let mut r = Record::from_generated(&mmx::txin_t::default())?;
                    let output =
                        Record::from_generated(&io(i.address, i.contract, i.amount, &i.memo))?;
                    r.fields.extend(output.fields);
                    r.fields
                        .insert("solution".into(), Value::Uint(i.solution.into()));
                    r.fields.insert("flags".into(), Value::Uint(i.flags.into()));
                    Ok(Value::Record(r))
                })
                .collect::<mmx_consensus::Result<Vec<_>>>()?;
            let outputs = self
                .outputs
                .iter()
                .map(|o| {
                    Ok(Value::Record(Record::from_generated(&io(
                        o.address, o.contract, o.amount, &o.memo,
                    ))?))
                })
                .collect::<mmx_consensus::Result<Vec<_>>>()?;
            let solutions = self
                .solutions
                .iter()
                .map(|s| {
                    let mut r = Record::from_generated(&mmx::solution::PubKey::default())?;
                    r.fields
                        .insert("version".into(), Value::Uint(s.version.into()));
                    r.fields
                        .insert("pubkey".into(), Value::Bytes(s.pubkey.to_vec()));
                    r.fields
                        .insert("signature".into(), Value::Bytes(s.signature.to_vec()));
                    Ok(Value::Record(r))
                })
                .collect::<mmx_consensus::Result<Vec<_>>>()?;
            out.fields.insert("inputs".into(), Value::Sequence(inputs));
            out.fields
                .insert("outputs".into(), Value::Sequence(outputs));
            out.fields
                .insert("solutions".into(), Value::Sequence(solutions));
            Ok(out)
        };
        if !self.execute.is_empty()
            || self.deploy.is_some()
            || self.exec_result.is_some()
            || self.note != "TRANSFER"
            || self.version != 1
        {
            return Err(Error::InvalidTransaction(
                "only hardfork2 simple transfers are supported",
            ));
        }
        convert().map_err(|_| Error::InvalidTransaction("native transfer conversion failed"))
    }
}
impl TryFrom<&mmx::ChainParams> for ChainParams {
    type Error = Error;
    fn try_from(p: &mmx::ChainParams) -> Result<Self> {
        Ok(Self {
            network: p.network.clone(),
            decimals: p
                .decimals
                .try_into()
                .map_err(|_| Error::InvalidArgument("invalid native currency decimals".into()))?,
            min_txfee: p.min_txfee,
            min_txfee_io: p.min_txfee_io,
            min_txfee_sign: p.min_txfee_sign,
            min_txfee_memo: p.min_txfee_memo,
        })
    }
}
