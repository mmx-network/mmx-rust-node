//! The pre-HF2 consensus order of GCC/libstdc++'s unordered_map<addr_t,uint128>.
//! Hash is the first eight address bytes interpreted as little endian. Rehash
//! prepends bucket groups and reverses their encountered nodes. HF2 sorts keys.
use mmx_vm::Address;
use std::collections::{BTreeMap, VecDeque};
#[derive(Default)]
pub(super) struct Amounts {
    pub values: BTreeMap<Address, u128>,
    buckets: usize,
    groups: BTreeMap<usize, Vec<Address>>,
    order: VecDeque<usize>,
}
impl Amounts {
    fn place(&mut self, key: Address) {
        let bucket =
            (u64::from_le_bytes(key[..8].try_into().unwrap()) % self.buckets as u64) as usize;
        if !self.groups.contains_key(&bucket) {
            self.order.push_front(bucket);
        }
        self.groups.entry(bucket).or_default().push(key);
    }
    pub fn entry(&mut self, key: Address) -> &mut u128 {
        if !self.values.contains_key(&key) {
            if self.values.len() >= self.buckets {
                const PRIMES: &[usize] = &[
                    13, 29, 59, 127, 257, 541, 1109, 2357, 5087, 10273, 20753, 42043, 85229,
                    172933, 351061, 712697, 1447153, 2938679, 5967347, 12117689, 24607243,
                    49969847,
                ];
                let old = self.keys(false);
                self.buckets = *PRIMES
                    .iter()
                    .find(|p| **p > self.buckets)
                    .expect("decoder limits bound the currency count");
                self.groups.clear();
                self.order.clear();
                for key in old {
                    self.place(key);
                }
            }
            self.place(key);
        }
        self.values.entry(key).or_default()
    }
    pub fn keys(&self, sorted: bool) -> Vec<Address> {
        if sorted {
            self.values.keys().copied().collect()
        } else {
            self.order
                .iter()
                .flat_map(|b| self.groups[b].iter().rev().copied())
                .collect()
        }
    }
}
