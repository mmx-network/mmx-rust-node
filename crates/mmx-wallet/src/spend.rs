//! General native transaction completion. The lossless consensus record retains
//! contract/Variant arguments; wallet presentation JSON is never hashed here.
use crate::{sha256, Address, Error, Result, Wallet};
use mmx_consensus::{Record, Value as V};
use mmx_types::mmx;
use rand::{rngs::OsRng, RngCore};
use std::collections::BTreeMap;

pub type Balances = BTreeMap<(Address, Address), u128>;
fn invalid(s: &'static str) -> Error {
    Error::InvalidTransaction(s)
}
fn address(v: &V) -> Result<Address> {
    Ok(Address(
        v.bytes()?
            .try_into()
            .map_err(|_| invalid("address length"))?,
    ))
}
fn amount(r: &Record) -> Result<u128> {
    Ok(u128::from_le_bytes(
        r.field("amount")?
            .bytes()?
            .try_into()
            .map_err(|_| invalid("amount length"))?,
    ))
}
fn sum(map: &mut BTreeMap<Address, u128>, key: Address, value: u128) -> Result<()> {
    let n = map.entry(key).or_default();
    *n = n.checked_add(value).ok_or(invalid("amount overflow"))?;
    Ok(())
}
fn put(r: &mut Record, name: &str, n: u64) {
    r.fields.insert(name.into(), V::Uint(n));
}
fn hash(r: &mut Record, full: bool) -> Result<()> {
    let h = r.calc_hash(full, 0)?;
    r.fields.insert(
        if full { "content_hash" } else { "id" }.into(),
        V::Bytes(h.to_vec()),
    );
    Ok(())
}
fn fee(cost: u64, ratio: u32) -> Result<u32> {
    (u128::from(cost) * u128::from(ratio) / 1024)
        .try_into()
        .map_err(|_| invalid("fee exceeds 32 bits"))
}
impl Wallet {
    fn plan(
        &self,
        tx: &mut Record,
        options: &mmx::spend_options_t,
    ) -> Result<BTreeMap<usize, Address>> {
        let mut solutions = tx.field("solutions")?.seq()?.to_vec();
        let mut owners = BTreeMap::new();
        let mut signing = BTreeMap::new();
        for (i, s) in solutions.iter().enumerate() {
            let s = s.record()?;
            if s.name == "mmx.solution.PubKey" {
                let owner = Address(sha256(s.field("pubkey")?.bytes()?));
                owners.insert(owner, i);
                if self.addresses.contains(&owner) {
                    signing.insert(i, owner);
                }
            }
        }
        let mut add = |owner: Address| -> Result<Option<u16>> {
            if !self.addresses.contains(&owner) {
                return Ok(None);
            }
            let index = if let Some(&i) = owners.get(&owner) {
                i
            } else {
                if solutions.len() >= 65535 {
                    return Err(invalid("too many solutions"));
                }
                let i = solutions.len();
                let key = self.addresses.iter().position(|a| *a == owner).unwrap();
                let s = mmx::solution::PubKey {
                    pubkey: self.public_key(key)?.into(),
                    ..Default::default()
                };
                solutions.push(V::Record(Record::from_generated(&s)?));
                owners.insert(owner, i);
                signing.insert(i, owner);
                i
            };
            Ok(Some(index as u16))
        };
        if tx.field("solutions")?.seq()?.is_empty() {
            if let Some(sender) = tx.field("sender")?.optional()? {
                add(address(sender)?)?;
            }
        }
        let mut inputs = tx.field("inputs")?.seq()?.to_vec();
        for input in &mut inputs {
            let V::Record(input) = input else {
                return Err(invalid("input record"));
            };
            if input.field("solution")?.uint()? != 65535 {
                continue;
            }
            let owner = address(input.field("address")?)?;
            let owner = if let Some(owner) = options.owner_map.get(&owner.0.into()) {
                put(input, "flags", input.field("flags")?.uint()? | 1);
                Address(owner.0)
            } else {
                owner
            };
            if let Some(i) = add(owner)? {
                put(input, "solution", i.into());
            }
        }
        let mut ops = tx.field("execute")?.seq()?.to_vec();
        for op in &mut ops {
            let V::Record(op) = op else {
                return Err(invalid("operation record"));
            };
            if op.field("solution")?.uint()? != 65535 {
                continue;
            }
            let owner = if matches!(
                op.name.as_str(),
                "mmx.operation.Execute" | "mmx.operation.Deposit"
            ) {
                op.field("user")?.optional()?.map(address).transpose()?
            } else {
                let a = address(op.field("address")?)?;
                Some(Address(
                    options.owner_map.get(&a.0.into()).map_or(a.0, |v| v.0),
                ))
            };
            if let Some(owner) = owner {
                if let Some(i) = add(owner)? {
                    put(op, "solution", i.into());
                }
            }
        }
        tx.fields.insert("inputs".into(), V::Sequence(inputs));
        tx.fields.insert("execute".into(), V::Sequence(ops));
        tx.fields.insert("solutions".into(), V::Sequence(solutions));
        Ok(signing)
    }
    fn finish(&self, tx: &mut Record, signing: BTreeMap<usize, Address>) -> Result<()> {
        hash(tx, false)?;
        let id: [u8; 32] = tx
            .field("id")?
            .bytes()?
            .try_into()
            .map_err(|_| invalid("id length"))?;
        let mut solutions = tx.field("solutions")?.seq()?.to_vec();
        for (i, owner) in signing {
            let sig = self.sign_message(owner, &id)?;
            solutions[i] = V::Record(Record::from_generated(&mmx::solution::PubKey {
                pubkey: sig.pubkey.into(),
                signature: sig.signature.into(),
                ..Default::default()
            })?);
        }
        tx.fields.insert("solutions".into(), V::Sequence(solutions));
        hash(tx, true)
    }
    /// Sign known sender/input/operation owners, leaving foreign inputs available
    /// for subsequent partial signing. Existing owned signatures are refreshed.
    pub fn sign_native(
        &self,
        tx: &Record,
        params: &mmx::ChainParams,
        options: &mmx::spend_options_t,
    ) -> Result<Record> {
        if tx.name != "mmx.Transaction" || tx.field("exec_result")?.optional()?.is_some() {
            return Err(invalid("expected unsigned execution receipt"));
        }
        let mut tx = tx.clone();
        tx.fields.insert(
            "network".into(),
            V::String(params.network.as_bytes().to_vec()),
        );
        if let Some(nonce) = options.nonce {
            put(&mut tx, "nonce", nonce);
        }
        let signing = self.plan(&mut tx, options)?;
        let cost = tx.calc_cost(&params.into())?;
        put(
            &mut tx,
            "static_cost",
            u32::try_from(cost)
                .map_err(|_| invalid("static cost exceeds 32 bits"))?
                .into(),
        );
        self.finish(&mut tx, signing)?;
        Ok(tx)
    }
    /// Fund outputs and deposits using confirmed, reservation-adjusted balances.
    /// Includes final signature costs before fixing the signed maximum fee.
    pub fn complete_native(
        &self,
        tx: &Record,
        balances: &Balances,
        params: &mmx::ChainParams,
        height: u32,
        options: &mmx::spend_options_t,
    ) -> Result<Record> {
        if tx.name != "mmx.Transaction"
            || tx.field("exec_result")?.optional()?.is_some()
            || !tx.field("solutions")?.seq()?.is_empty()
        {
            return Err(invalid(
                "complete requires a transaction without signatures or receipt",
            ));
        }
        if options.fee_ratio < 1024 {
            return Err(invalid("fee ratio below minimum"));
        }
        let mut tx = tx.clone();
        put(
            &mut tx,
            "version",
            u64::from(
                height.checked_add(1).ok_or(invalid("height overflow"))? >= params.hardfork2_height,
            ),
        );
        tx.fields.insert(
            "network".into(),
            V::String(params.network.as_bytes().to_vec()),
        );
        put(
            &mut tx,
            "nonce",
            options.nonce.unwrap_or_else(|| OsRng.next_u64()),
        );
        let expires = options
            .expire_at
            .unwrap_or_else(|| height.saturating_add(options.expire_delta.unwrap_or(100)));
        if expires <= height {
            return Err(invalid("transaction already expired"));
        }
        let old_expires = tx.field("expires")?.uint()?;
        put(&mut tx, "expires", old_expires.min(expires.into()));
        let ratio: u32 = tx
            .field("fee_ratio")?
            .uint()?
            .max(options.fee_ratio.into())
            .try_into()
            .map_err(|_| invalid("fee ratio overflow"))?;
        put(&mut tx, "fee_ratio", ratio.into());
        if let Some(note) = &options.note {
            put(&mut tx, "note", note.value.into());
        }
        let mut required = BTreeMap::new();
        for value in tx.field("outputs")?.seq()? {
            let r = value.record()?;
            if amount(r)? == 0 || address(r.field("address")?)? == Address::default() {
                return Err(invalid("zero output amount/address"));
            }
            sum(&mut required, address(r.field("contract")?)?, amount(r)?)?;
        }
        for value in tx.field("execute")?.seq()? {
            let r = value.record()?;
            if r.name == "mmx.operation.Deposit" {
                sum(&mut required, address(r.field("currency")?)?, amount(r)?)?;
            }
        }
        let mut inputs = tx.field("inputs")?.seq()?.to_vec();
        let mut funded = BTreeMap::new();
        let mut spent: Balances = BTreeMap::new();
        for value in &inputs {
            let r = value.record()?;
            let owner = address(r.field("address")?)?;
            let currency = address(r.field("contract")?)?;
            let n = amount(r)?;
            sum(&mut funded, currency, n)?;
            let value = spent.entry((owner, currency)).or_default();
            *value = value.checked_add(n).ok_or(invalid("input overflow"))?;
        }
        for (&(owner, currency), &n) in &spent {
            if self.addresses.contains(&owner)
                && n > balances.get(&(owner, currency)).copied().unwrap_or(0)
            {
                return Err(Error::InsufficientFunds("existing transaction input"));
            }
        }
        for (currency, n) in required {
            let mut missing = n.saturating_sub(funded.get(&currency).copied().unwrap_or(0));
            let mut candidates: Vec<_> = balances
                .iter()
                .filter(|((a, c), _)| *c == currency && self.addresses.contains(a))
                .map(|(&(a, c), &n)| {
                    (
                        n.saturating_sub(spent.get(&(a, c)).copied().unwrap_or(0)),
                        a,
                    )
                })
                .collect();
            candidates.sort_by(|a, b| b.cmp(a));
            for (available, owner) in candidates {
                let take = missing.min(available);
                if take == 0 {
                    continue;
                }
                let mut input = Record::from_generated(&mmx::txin_t::default())?;
                input
                    .fields
                    .insert("address".into(), V::Bytes(owner.0.to_vec()));
                input
                    .fields
                    .insert("contract".into(), V::Bytes(currency.0.to_vec()));
                input
                    .fields
                    .insert("amount".into(), V::Bytes(take.to_le_bytes().to_vec()));
                inputs.push(V::Record(input));
                *spent.entry((owner, currency)).or_default() += take;
                missing -= take;
            }
            if missing > 0 {
                return Err(Error::InsufficientFunds("outputs and deposits"));
            }
        }
        tx.fields.insert("inputs".into(), V::Sequence(inputs));
        let payer = options
            .sender
            .as_ref()
            .map(|a| Address(a.0))
            .or_else(|| {
                balances
                    .iter()
                    .filter(|((a, c), _)| *c == Address::default() && self.addresses.contains(a))
                    .max_by_key(|(&(a, c), &n)| {
                        (
                            n.saturating_sub(spent.get(&(a, c)).copied().unwrap_or(0)),
                            a,
                        )
                    })
                    .map(|(&(a, _), _)| a)
            })
            .ok_or(Error::InsufficientFunds("transaction fee"))?;
        if !self.addresses.contains(&payer) {
            return Err(invalid("fee payer not owned by wallet"));
        }
        tx.fields.insert(
            "sender".into(),
            V::Optional(Some(Box::new(V::Bytes(payer.0.to_vec())))),
        );
        let signing = self.plan(&mut tx, options)?;
        let cost = tx.calc_cost(&params.into())?;
        put(
            &mut tx,
            "static_cost",
            u32::try_from(cost)
                .map_err(|_| invalid("static cost exceeds 32 bits"))?
                .into(),
        );
        let static_fee = fee(cost, ratio)?;
        if balances
            .get(&(payer, Address::default()))
            .copied()
            .unwrap_or(0)
            .saturating_sub(
                spent
                    .get(&(payer, Address::default()))
                    .copied()
                    .unwrap_or(0),
            )
            < static_fee.into()
        {
            return Err(Error::InsufficientFunds("transaction fee"));
        }
        put(
            &mut tx,
            "max_fee_amount",
            fee(
                cost.checked_add(options.gas_limit.into())
                    .ok_or(invalid("cost overflow"))?,
                ratio,
            )?
            .into(),
        );
        self.finish(&mut tx, signing)?;
        if !tx.is_valid(&params.into())? {
            return Err(invalid("native structural validation failed"));
        }
        Ok(tx)
    }
}
