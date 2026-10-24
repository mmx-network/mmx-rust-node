//! Transfer fees and the largest amount `Wallet::transfer` can send.
use crate::{transaction::Selection, Address, Balances, ChainParams, Error, Result, Wallet};
use std::collections::BTreeSet;

/// Static cost of a simple transfer with `inputs` inputs, one output carrying
/// `memo` and `owners` distinct signatures, exactly as `Transaction::calc_cost`
/// computes it. This is the fee at the minimum fee ratio (1024); a transfer
/// signed with fee ratio `r` pays `static_fee(..) * r / 1024`.
pub fn static_fee(params: &ChainParams, inputs: usize, owners: usize, memo: Option<&str>) -> u128 {
    let io = (inputs as u128).saturating_add(1);
    let memo_units = memo.map_or(0, |m| m.len().div_ceil(32).max(1)) as u128;
    u128::from(params.min_txfee)
        .saturating_add(io.saturating_mul(params.min_txfee_io.into()))
        .saturating_add(memo_units.saturating_mul(params.min_txfee_memo.into()))
        .saturating_add((owners as u128).saturating_mul(params.min_txfee_sign.into()))
}

/// Fee for `cost` at `fee_ratio`, rounded down like the node's `cost_to_fee`.
pub(crate) fn cost_to_fee(cost: u128, fee_ratio: u32) -> u128 {
    cost.saturating_mul(fee_ratio.into()) / 1024
}

impl Wallet {
    /// Whether the selected fee payer keeps the static fee after its own inputs.
    fn covers_fee(
        selection: &Selection,
        balances: &Balances,
        params: &ChainParams,
        currency: Address,
        memo: Option<&str>,
        fee_ratio: u32,
    ) -> bool {
        let native = Address::default();
        let cost = static_fee(params, selection.inputs.len(), selection.owners.len(), memo);
        // `transfer` rejects a static cost that does not fit the signed field.
        if cost > u128::from(u32::MAX) {
            return false;
        }
        let spent = if currency == native {
            selection
                .inputs
                .iter()
                .find(|(address, ..)| *address == selection.sender)
                .map_or(0, |&(_, amount, _)| amount)
        } else {
            0
        };
        let balance = balances
            .get(&(selection.sender, native))
            .copied()
            .unwrap_or(0);
        spent
            .checked_add(cost_to_fee(cost, fee_ratio))
            .is_some_and(|needed| balance >= needed)
    }

    /// Largest amount of `currency` that `transfer` accepts with these balances,
    /// memo and fee ratio while its fee payer keeps the fee.
    ///
    /// Native MMX pays its own fee, so every arrangement of inputs and fee payer
    /// that `transfer` can produce is considered; the result is 0 when nothing
    /// can be sent. Tokens: the whole owned token balance, or
    /// `InsufficientFunds("transaction fee")` when the fee payer (the owned
    /// address with the most MMX) cannot cover the fee.
    pub fn max_transfer_amount(
        &self,
        balances: &Balances,
        params: &ChainParams,
        currency: Address,
        memo: Option<&str>,
        fee_ratio: u32,
    ) -> Result<u128> {
        if memo.is_some_and(|m| m.len() > 64) || fee_ratio < 1024 {
            return Err(Error::InvalidArgument("invalid transfer options".into()));
        }
        let native = Address::default();
        let owned: BTreeSet<&Address> = self.addresses.iter().collect();
        // Selection only looks at owned, nonzero balances of the sent currency
        // and of MMX; dropping the rest keeps the search cheap and identical.
        let relevant: Balances = balances
            .iter()
            .filter(|((a, c), n)| **n > 0 && owned.contains(a) && (*c == currency || *c == native))
            .map(|(k, v)| (*k, *v))
            .collect();
        let sendable = |amount: u128| -> Result<bool> {
            match self.select(&relevant, currency, amount) {
                Ok(selection) => Ok(Self::covers_fee(
                    &selection, &relevant, params, currency, memo, fee_ratio,
                )),
                Err(Error::InsufficientFunds(_)) => Ok(false),
                Err(e) => Err(e),
            }
        };
        let mut amounts: Vec<u128> = relevant
            .iter()
            .filter(|((_, c), _)| *c == currency)
            .map(|(_, n)| *n)
            .collect();
        if currency != native {
            let total = amounts
                .iter()
                .try_fold(0u128, |sum, n| sum.checked_add(*n))
                .ok_or_else(|| Error::InvalidArgument("token balance overflow".into()))?;
            if total == 0 {
                return Ok(0);
            }
            return if sendable(total)? {
                Ok(total)
            } else {
                Err(Error::InsufficientFunds("transaction fee"))
            };
        }
        // With the j largest balances as inputs (the last one possibly partial),
        // the best amount is either all of them (fee paid by the next balance)
        // or all minus the smallest remainder that keeps the last input as a fee
        // payer: its fee, the next balance (+1 for map-order ties) or 1.
        amounts.sort_unstable_by(|a, b| b.cmp(a));
        let mut candidates = BTreeSet::new();
        let mut prefix = 0u128;
        for (j, balance) in amounts.iter().enumerate() {
            let Some(sum) = prefix.checked_add(*balance) else {
                break;
            };
            prefix = sum;
            let next = amounts.get(j + 1).copied().unwrap_or(0);
            let fee = cost_to_fee(static_fee(params, j + 1, j + 1, memo), fee_ratio);
            for keep in [0, 1, fee, next, next.saturating_add(1)] {
                if let Some(amount) = prefix.checked_sub(keep).filter(|a| *a > 0) {
                    candidates.insert(amount);
                }
            }
        }
        for amount in candidates.into_iter().rev() {
            if sendable(amount)? {
                return Ok(amount);
            }
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Output, TransferOptions};

    const NATIVE: Address = Address([0; 32]);
    const MEMOS: [Option<&str>; 6] = [
        None,
        Some(""),
        Some("a"),
        Some("0123456789abcdef0123456789abcdef"),
        Some("0123456789abcdef0123456789abcdef!"),
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
    ];

    /// Deterministic 64-bit LCG (Knuth MMIX constants).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn pick<T: Copy>(&mut self, items: &[T]) -> T {
            items[self.below(items.len() as u64) as usize]
        }
    }

    fn wallet(count: u32) -> Wallet {
        let seed = std::array::from_fn(|i| (i * 7 + 3) as u8);
        Wallet::derive(&seed, "", 0, count).unwrap()
    }

    fn fee_of(
        params: &ChainParams,
        inputs: usize,
        owners: usize,
        memo: Option<&str>,
        ratio: u32,
    ) -> u128 {
        static_fee(params, inputs, owners, memo) * u128::from(ratio) / 1024
    }

    /// Independent definition of "transfer would be accepted and its fee payer
    /// keeps the fee", using the selection `transfer` signs.
    fn valid(
        w: &Wallet,
        balances: &Balances,
        params: &ChainParams,
        currency: Address,
        memo: Option<&str>,
        ratio: u32,
        amount: u128,
    ) -> bool {
        let Ok(sel) = w.select(balances, currency, amount) else {
            return false;
        };
        let fee = fee_of(params, sel.inputs.len(), sel.owners.len(), memo, ratio);
        let spent: u128 = sel
            .inputs
            .iter()
            .filter(|i| i.0 == sel.sender && currency == NATIVE)
            .map(|i| i.1)
            .sum();
        balances.get(&(sel.sender, NATIVE)).copied().unwrap_or(0) >= spent + fee
    }

    fn transfer(
        w: &Wallet,
        balances: &Balances,
        params: &ChainParams,
        currency: Address,
        amount: u128,
        memo: Option<&str>,
        ratio: u32,
    ) -> Result<crate::Transaction> {
        w.transfer(
            balances,
            params,
            1000,
            Output {
                address: Address([9; 32]),
                contract: currency,
                amount,
                memo: memo.map(str::to_owned),
            },
            &TransferOptions {
                fee_ratio: ratio,
                nonce: Some(42),
                ..Default::default()
            },
        )
    }

    fn remaining_native(tx: &crate::Transaction, balances: &Balances) -> u128 {
        let sender = tx.sender.unwrap();
        let spent: u128 = tx
            .inputs
            .iter()
            .filter(|i| i.address == sender && i.contract == NATIVE)
            .map(|i| i.amount)
            .sum();
        balances.get(&(sender, NATIVE)).copied().unwrap_or(0) - spent
    }

    #[test]
    fn static_fee_matches_transaction_cost() {
        let w = wallet(4);
        let token = Address([5; 32]);
        let mut rng = Lcg(7);
        for _ in 0..40 {
            let params = ChainParams {
                min_txfee: rng.below(500),
                min_txfee_io: rng.below(500),
                min_txfee_sign: rng.below(5000),
                min_txfee_memo: rng.below(200),
                ..Default::default()
            };
            let memo = rng.pick(&MEMOS);
            let currency = rng.pick(&[NATIVE, token]);
            let mut balances = Balances::new();
            for a in &w.addresses {
                balances.insert((*a, currency), 1 + u128::from(rng.below(1000)));
                balances.insert((*a, NATIVE), 1 + u128::from(rng.below(1000)));
            }
            let total: u128 = balances
                .iter()
                .filter(|((_, c), _)| *c == currency)
                .map(|(_, n)| *n)
                .sum();
            let amount = 1 + u128::from(rng.below(total as u64));
            let ratio = 1024 + rng.below(9 * 1024) as u32;
            let tx = transfer(&w, &balances, &params, currency, amount, memo, ratio).unwrap();
            let cost = static_fee(&params, tx.inputs.len(), tx.solutions.len(), memo);
            assert_eq!(cost, u128::from(tx.calc_cost(&params).unwrap()));
            assert_eq!(cost, u128::from(tx.static_cost));
            assert_eq!(tx.fee_amount(), cost * u128::from(ratio) / 1024);
        }
        let params = ChainParams::default();
        assert_eq!(static_fee(&params, 1, 1, None), 1300);
        assert_eq!(static_fee(&params, 1, 1, Some("")), 1350);
        assert_eq!(static_fee(&params, 2, 3, Some(MEMOS[4].unwrap())), 3500);
    }

    /// Exhaustive check against every amount for small balances: the result is
    /// the largest amount whose transfer leaves the fee payer holding the fee.
    #[test]
    fn native_maximum_is_the_largest_sendable_amount() {
        let w = wallet(5);
        let mut rng = Lcg(2024);
        for case in 0..600 {
            let params = ChainParams {
                min_txfee: rng.below(3),
                min_txfee_io: rng.below(3),
                min_txfee_sign: 1 + rng.below(6),
                min_txfee_memo: rng.below(3),
                ..Default::default()
            };
            let ratio = rng.pick(&[1024, 1536, 2048, 3072, 5120]);
            let memo = rng.pick(&MEMOS);
            let funded = 1 + rng.below(5) as usize;
            let mut balances = Balances::new();
            let mut last = 0;
            for a in w.addresses.iter().take(funded) {
                // Repeat the previous balance now and then to exercise ties.
                let n = if case % 3 == 0 && last > 0 && rng.below(2) == 0 {
                    last
                } else {
                    u128::from(rng.below(45))
                };
                last = n;
                balances.insert((*a, NATIVE), n);
            }
            if rng.below(4) == 0 {
                balances.insert((Address([3; 32]), NATIVE), 1000); // not owned
            }
            let total: u128 = w
                .addresses
                .iter()
                .filter_map(|a| balances.get(&(*a, NATIVE)))
                .sum();
            let expected = (1..=total)
                .rev()
                .find(|a| valid(&w, &balances, &params, NATIVE, memo, ratio, *a))
                .unwrap_or(0);
            let max = w
                .max_transfer_amount(&balances, &params, NATIVE, memo, ratio)
                .unwrap();
            assert_eq!(
                max, expected,
                "case {case}: {balances:?} {params:?} {ratio} {memo:?}"
            );
        }
    }

    /// Randomized balance sets around the fee boundaries, checked through real
    /// signed transfers.
    #[test]
    fn maximum_is_accepted_by_transfer_and_leaves_the_fee() {
        let w = wallet(6);
        let params = ChainParams::default();
        let mut rng = Lcg(0x5eed);
        let mut nonzero = 0;
        for case in 0..160 {
            let ratio = rng.pick(&[1024, 2048, 3072, 5120, 10240, 1536]);
            let memo = rng.pick(&MEMOS);
            let funded = 1 + rng.below(6) as usize;
            let mut balances = Balances::new();
            let mut last = 0u128;
            for a in w.addresses.iter().take(funded) {
                // Fee of k inputs signed by k or k + 1 owners: the boundaries
                // where the last input or the next balance can pay the fee.
                let k = 1 + rng.below(funded as u64) as usize;
                let edge = fee_of(&params, k, k + rng.below(2) as usize, memo, ratio);
                let n = match rng.below(6) {
                    0 => u128::from(rng.below(5000)),
                    1 => edge + u128::from(rng.below(3)) - 1,
                    2 => edge + u128::from(rng.below(301)) - 150,
                    3 if last > 0 => last,
                    4 => u128::from(rng.next()) * 1_000_000 + u128::from(rng.next()),
                    _ => u128::from(rng.below(100_000)),
                };
                last = n;
                balances.insert((*a, NATIVE), n);
            }
            let max = w
                .max_transfer_amount(&balances, &params, NATIVE, memo, ratio)
                .unwrap();
            if max == 0 {
                if let Ok(tx) = transfer(&w, &balances, &params, NATIVE, 1, memo, ratio) {
                    assert!(
                        remaining_native(&tx, &balances) < tx.fee_amount(),
                        "case {case}"
                    );
                    assert!(!tx.fee_payer_covered(&balances));
                }
                continue;
            }
            nonzero += 1;
            let tx = transfer(&w, &balances, &params, NATIVE, max, memo, ratio)
                .unwrap_or_else(|e| panic!("case {case}: {e} {balances:?}"));
            tx.verify(&params).unwrap();
            let fee = tx.fee_amount();
            assert_eq!(
                fee,
                fee_of(&params, tx.inputs.len(), tx.solutions.len(), memo, ratio),
                "case {case}"
            );
            assert_eq!(
                u128::from(tx.static_cost) * u128::from(ratio) / 1024,
                fee,
                "case {case}"
            );
            assert!(
                remaining_native(&tx, &balances) >= fee,
                "case {case}: {balances:?}"
            );
            assert!(tx.fee_payer_covered(&balances));
            if let Ok(over) = transfer(&w, &balances, &params, NATIVE, max + 1, memo, ratio) {
                assert!(
                    remaining_native(&over, &balances) < over.fee_amount(),
                    "case {case}: max + 1 is sendable {balances:?}"
                );
                assert!(!over.fee_payer_covered(&balances));
            }
        }
        assert!(nonzero > 100);
    }

    #[test]
    fn last_input_is_kept_as_fee_payer_when_the_next_balance_cannot_pay() {
        // The next balance exceeds the single-input fee but not the fee it
        // would owe as an extra signer, so the last input must keep more.
        let w = wallet(2);
        let params = ChainParams::default();
        for (big, small) in [(0, 1), (1, 0)] {
            let balances = Balances::from([
                ((w.addresses[big], NATIVE), 1_000_000),
                ((w.addresses[small], NATIVE), 2000),
            ]);
            let max = w
                .max_transfer_amount(&balances, &params, NATIVE, None, 1024)
                .unwrap();
            // Equal remainders resolve to the first address in map order.
            let tie_to_big = w.addresses[big] < w.addresses[small];
            assert_eq!(max, if tie_to_big { 998_000 } else { 997_999 });
            let tx = transfer(&w, &balances, &params, NATIVE, max, None, 1024).unwrap();
            assert_eq!(tx.sender, Some(w.addresses[big]));
            assert!(tx.fee_payer_covered(&balances));
            assert!(
                transfer(&w, &balances, &params, NATIVE, max + 1, None, 1024)
                    .is_ok_and(|tx| !tx.fee_payer_covered(&balances))
            );
        }
    }

    #[test]
    fn next_balance_pays_the_fee_when_it_can() {
        let w = wallet(2);
        let params = ChainParams::default();
        let balances = Balances::from([
            ((w.addresses[0], NATIVE), 1_000_000),
            ((w.addresses[1], NATIVE), 2300),
        ]);
        let max = w
            .max_transfer_amount(&balances, &params, NATIVE, None, 1024)
            .unwrap();
        // One input and two signatures cost 2300: the whole larger balance moves.
        assert_eq!(max, 1_000_000);
        let tx = transfer(&w, &balances, &params, NATIVE, max, None, 1024).unwrap();
        assert_eq!(tx.sender, Some(w.addresses[1]));
        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(tx.fee_amount(), 2300);
    }

    #[test]
    fn single_address_and_edge_balances() {
        let w = wallet(1);
        let params = ChainParams::default();
        let a = w.addresses[0];
        for memo in MEMOS {
            for ratio in [1024, 2048, 3072, 5120, 10240] {
                let fee = fee_of(&params, 1, 1, memo, ratio);
                for (balance, expected) in [
                    (0, 0),
                    (fee - 1, 0),
                    (fee, 0),
                    (fee + 1, 1),
                    (fee + 123_456, 123_456),
                ] {
                    let balances = Balances::from([((a, NATIVE), balance)]);
                    let max = w
                        .max_transfer_amount(&balances, &params, NATIVE, memo, ratio)
                        .unwrap();
                    assert_eq!(max, expected, "{memo:?} {ratio} {balance}");
                    if max > 0 {
                        let tx =
                            transfer(&w, &balances, &params, NATIVE, max, memo, ratio).unwrap();
                        assert_eq!(remaining_native(&tx, &balances), fee);
                    }
                }
            }
        }
        assert_eq!(
            w.max_transfer_amount(&Balances::new(), &params, NATIVE, None, 1024)
                .unwrap(),
            0
        );
    }

    #[test]
    fn rejects_invalid_options() {
        let w = wallet(1);
        let params = ChainParams::default();
        let balances = Balances::from([((w.addresses[0], NATIVE), 1_000_000)]);
        let long = "x".repeat(65);
        assert!(matches!(
            w.max_transfer_amount(&balances, &params, NATIVE, Some(&long), 1024),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            w.max_transfer_amount(&balances, &params, NATIVE, None, 1023),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn token_maximum_is_the_full_balance_paid_by_the_largest_mmx_holder() {
        let w = wallet(4);
        let params = ChainParams::default();
        let token = Address([7; 32]);
        let mut rng = Lcg(99);
        for case in 0..60 {
            let memo = rng.pick(&MEMOS);
            let ratio = rng.pick(&[1024, 2048, 3072, 5120, 10240]);
            let mut balances = Balances::new();
            for a in &w.addresses {
                if rng.below(3) > 0 {
                    balances.insert((*a, token), u128::from(rng.below(1_000_000)));
                }
                if rng.below(2) > 0 {
                    balances.insert((*a, NATIVE), u128::from(rng.below(60_000)));
                }
            }
            balances.insert((Address([3; 32]), token), 5); // not owned
            let total: u128 = w
                .addresses
                .iter()
                .filter_map(|a| balances.get(&(*a, token)))
                .sum();
            let result = w.max_transfer_amount(&balances, &params, token, memo, ratio);
            if total == 0 {
                assert_eq!(result.unwrap(), 0);
                continue;
            }
            let tx = transfer(&w, &balances, &params, token, total, memo, ratio).unwrap();
            let payer = balances
                .iter()
                .filter(|((a, c), _)| *c == NATIVE && w.addresses.contains(a))
                .map(|(_, n)| *n)
                .max()
                .unwrap_or(0);
            if payer > 0 {
                assert_eq!(balances[&(tx.sender.unwrap(), NATIVE)], payer);
            }
            if tx.fee_payer_covered(&balances) {
                assert_eq!(result.unwrap(), total, "case {case}");
                assert_eq!(
                    tx.fee_amount(),
                    fee_of(&params, tx.inputs.len(), tx.solutions.len(), memo, ratio)
                );
                assert!(remaining_native(&tx, &balances) >= tx.fee_amount());
            } else {
                assert!(
                    matches!(result, Err(Error::InsufficientFunds("transaction fee"))),
                    "case {case}"
                );
            }
        }
    }

    #[test]
    fn token_fee_counts_the_payer_signature() {
        let w = wallet(2);
        let params = ChainParams::default();
        let token = Address([7; 32]);
        // One token input plus a separate MMX payer: 1 input, 2 signatures.
        let fee = fee_of(&params, 1, 2, None, 1024);
        assert_eq!(fee, 2300);
        for (mmx, ok) in [(fee - 1, false), (fee, true)] {
            let balances = Balances::from([
                ((w.addresses[0], token), 500),
                ((w.addresses[1], NATIVE), mmx),
            ]);
            let result = w.max_transfer_amount(&balances, &params, token, None, 1024);
            if ok {
                assert_eq!(result.unwrap(), 500);
            } else {
                assert!(matches!(
                    result,
                    Err(Error::InsufficientFunds("transaction fee"))
                ));
            }
        }
        // The payer also holds the token: a single signature.
        let balances = Balances::from([
            ((w.addresses[1], token), 500),
            ((w.addresses[1], NATIVE), fee_of(&params, 1, 1, None, 1024)),
        ]);
        assert_eq!(
            w.max_transfer_amount(&balances, &params, token, None, 1024)
                .unwrap(),
            500
        );
        let no_mmx = Balances::from([((w.addresses[0], token), 500)]);
        assert!(matches!(
            w.max_transfer_amount(&no_mmx, &params, token, None, 1024),
            Err(Error::InsufficientFunds("transaction fee"))
        ));
    }
}
