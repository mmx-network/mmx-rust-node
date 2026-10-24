//! Funds of the wallet's own broadcast transfers that are not in a block yet,
//! as C++ `ECDSA_Wallet::update_from` / `update_cache` keep them: the fee
//! payer's static fee and every input stay reserved until the transfer is in
//! a block (executed or failed) or has expired, so the next transfer neither
//! offers nor spends them again.
use crate::remote::TxStatus;
use mmx_wallet::{Address, Balances, Transaction};
use std::collections::BTreeMap;

/// Pending transfers kept per wallet; the oldest are dropped first.
pub const MAX_PER_WALLET: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    /// Lowercase hexadecimal transaction ID.
    pub txid: String,
    /// Network the transfer was made for. Addresses are the same on every
    /// network, so only balances read from this one are reduced.
    pub network: String,
    /// Last block height that can include the transfer.
    pub expires: u32,
    /// Chain height at which the transfer was last known not to be in a
    /// block; it cannot be in one before the chain grows past it.
    pub checked: u32,
    /// (owner, currency) → amount.
    pub amounts: Vec<((Address, Address), u128)>,
}

impl Reservation {
    /// The sender's static fee and all inputs of a transfer for `network`,
    /// last known not to be in a block at `height`.
    pub fn new(tx: &Transaction, network: &str, height: u32) -> Self {
        let mut amounts: BTreeMap<(Address, Address), u128> = BTreeMap::new();
        let mut add = |key, amount: u128| {
            let entry = amounts.entry(key).or_default();
            *entry = entry.saturating_add(amount);
        };
        if let Some(sender) = tx.sender {
            add((sender, Address::default()), tx.fee_amount());
        }
        for input in &tx.inputs {
            add((input.address, input.contract), input.amount);
        }
        Self {
            txid: tx.id_hex(),
            network: network.to_owned(),
            expires: tx.expires,
            checked: height,
            amounts: amounts.into_iter().collect(),
        }
    }
}

/// Adds a transfer (once), dropping the oldest beyond the limit.
pub fn add(list: &mut Vec<Reservation>, reservation: Reservation) {
    if list.iter().any(|r| r.txid == reservation.txid) {
        return;
    }
    list.push(reservation);
    if list.len() > MAX_PER_WALLET {
        list.drain(..list.len() - MAX_PER_WALLET);
    }
}

/// Drops transfers for `network` that expired before `height` (as C++ does)
/// and returns the IDs worth asking its chain server about: those not
/// checked at this height yet. Other networks' transfers are left alone.
pub fn due(list: &mut Vec<Reservation>, network: &str, height: u32) -> Vec<String> {
    list.retain(|r| r.network != network || r.expires >= height);
    list.iter()
        .filter(|r| r.network == network && r.checked < height)
        .map(|r| r.txid.clone())
        .collect()
}

/// Applies the server's answers for `txids` (balances read at `height`).
/// A transfer in a block at or below `height` is part of those balances and
/// is dropped; one in a later block stays reserved until the balances show
/// it; unknown answers keep everything as it was.
pub fn update(list: &mut Vec<Reservation>, txids: &[String], statuses: &[TxStatus], height: u32) {
    for (txid, status) in txids.iter().zip(statuses) {
        match *status {
            TxStatus::Included(at) if at <= height => list.retain(|r| r.txid != *txid),
            TxStatus::NotIncluded => {
                if let Some(r) = list.iter_mut().find(|r| r.txid == *txid) {
                    r.checked = r.checked.max(height);
                }
            }
            TxStatus::Included(_) | TxStatus::Unavailable => (),
        }
    }
}

/// Subtracts the amounts reserved on `network` from fresh balances of that
/// network, never below zero.
pub fn apply(balances: &mut Balances, list: &[Reservation], network: &str) {
    for reservation in list.iter().filter(|r| r.network == network) {
        for (key, amount) in &reservation.amounts {
            if let Some(balance) = balances.get_mut(key) {
                *balance = balance.saturating_sub(*amount);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(n: u8) -> Address {
        Address([n; 32])
    }

    fn reservation(txid: &str, expires: u32, checked: u32, amounts: &[(u8, u128)]) -> Reservation {
        Reservation {
            txid: txid.into(),
            network: "mainnet".into(),
            expires,
            checked,
            amounts: amounts
                .iter()
                .map(|&(a, amount)| ((address(a), Address::default()), amount))
                .collect(),
        }
    }

    #[test]
    fn reserved_amounts_are_subtracted_without_underflow() {
        let mut balances = Balances::from([
            ((address(1), Address::default()), 10),
            ((address(2), Address::default()), 5),
        ]);
        apply(
            &mut balances,
            &[
                reservation("a", 100, 0, &[(1, 4), (2, 9)]),
                reservation("b", 100, 0, &[(1, 3), (3, 7)]),
            ],
            "mainnet",
        );
        assert_eq!(
            balances,
            Balances::from([
                ((address(1), Address::default()), 3),
                ((address(2), Address::default()), 0),
            ])
        );
    }

    #[test]
    fn expiry_inclusion_and_failed_reads() {
        let mut list = vec![
            reservation("old", 99, 90, &[(1, 1)]),
            reservation("edge", 100, 90, &[(1, 1)]),
            reservation("seen", 200, 100, &[(1, 1)]),
            reservation("mined", 200, 90, &[(1, 1)]),
            reservation("later", 200, 90, &[(1, 1)]),
            reservation("lost", 200, 90, &[(1, 1)]),
        ];
        // Expired below the height (C++: expires < height); already checked
        // at this height: not asked again.
        let due = due(&mut list, "mainnet", 100);
        assert_eq!(due, ["edge", "mined", "later", "lost"]);
        update(
            &mut list,
            &due,
            &[
                TxStatus::NotIncluded,
                TxStatus::Included(100),
                TxStatus::Included(101),
                TxStatus::Unavailable,
            ],
            100,
        );
        let left: Vec<(&str, u32)> = list.iter().map(|r| (r.txid.as_str(), r.checked)).collect();
        assert_eq!(
            left,
            [("edge", 100), ("seen", 100), ("later", 90), ("lost", 90)]
        );
    }

    #[test]
    fn transfers_are_added_once_and_bounded() {
        let mut list = Vec::new();
        add(&mut list, reservation("a", 1, 0, &[]));
        add(&mut list, reservation("a", 2, 0, &[]));
        assert_eq!(list.len(), 1);
        for i in 0..MAX_PER_WALLET {
            add(&mut list, reservation(&format!("t{i}"), 1, 0, &[]));
        }
        assert_eq!(list.len(), MAX_PER_WALLET);
        assert_eq!(list[0].txid, "t0");
    }

    #[test]
    fn other_networks_are_left_alone() {
        let mut other = reservation("other", 1_000_100, 1_000_000, &[(1, 4)]);
        other.network = "testnet".into();
        let mut list = vec![other, reservation("main", 100, 90, &[(1, 3)])];
        // At a lower height on another network, nothing of it expires,
        // is looked up or is subtracted.
        assert_eq!(due(&mut list, "mainnet", 100), ["main"]);
        let mut balances = Balances::from([((address(1), Address::default()), 10)]);
        apply(&mut balances, &list, "mainnet");
        assert_eq!(balances[&(address(1), Address::default())], 7);
        // Back on its own network it still counts, until it expires there.
        assert!(due(&mut list, "testnet", 1_000_000).is_empty());
        let mut balances = Balances::from([((address(1), Address::default()), 10)]);
        apply(&mut balances, &list, "testnet");
        assert_eq!(balances[&(address(1), Address::default())], 6);
        due(&mut list, "testnet", 1_000_101);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].txid, "main");
    }
}
