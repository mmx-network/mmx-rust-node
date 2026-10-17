//! Native Node::find_best_fork policy. Inputs contain already verified proof
//! flags and validator vote counts; this module does not authenticate votes.
use mmx_consensus::Hash;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub hash: Hash,
    pub total_weight: u128,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub hash: Hash,
    pub prev: Hash,
    pub height: u32,
    pub total_weight: u128,
    pub votes: u32,
    pub proof_verified: bool,
    pub invalid: bool,
    /// Resolved parent at the root's height, used only for root-height + 1.
    pub anchor: Option<Anchor>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub anchor: Option<Anchor>,
    pub total_votes: u32,
    pub all_proofs_verified: bool,
    pub invalid: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub best: Option<Hash>,
    pub states: BTreeMap<Hash, State>,
}

/// Candidates are supplied in arrival order. Native multimap traversal sorts
/// by height and retains insertion order at equal heights. `sync_pending` is
/// the lowest pending sync height, including a backwards-sync request.
pub fn select(
    root_height: u32,
    root: Hash,
    sync_pending: Option<u32>,
    candidates: &[Candidate],
) -> Selection {
    let mut ordered: Vec<_> = candidates
        .iter()
        .filter(|c| c.height > root_height)
        .collect();
    ordered.sort_by_key(|c| c.height);
    let mut out = Selection::default();
    let mut best: Option<(&Candidate, State)> = None;
    for candidate in ordered {
        let mut state = State {
            invalid: candidate.invalid,
            ..Default::default()
        };
        if root_height.checked_add(1) == Some(candidate.height) {
            state.anchor = candidate.anchor;
            state.total_votes = candidate.votes;
            state.all_proofs_verified = candidate.proof_verified;
        } else if let Some(previous) = out.states.get(&candidate.prev) {
            state.anchor = previous.anchor;
            state.total_votes = previous.total_votes.wrapping_add(candidate.votes);
            state.invalid |= previous.invalid;
            state.all_proofs_verified = previous.all_proofs_verified && candidate.proof_verified;
        }
        out.states.insert(candidate.hash, state);
        let Some(anchor) = state.anchor else { continue };
        if anchor.hash != root && sync_pending.is_some_and(|height| height < root_height) {
            continue;
        }
        if !state.all_proofs_verified || state.invalid {
            continue;
        }
        let replace = match best {
            None => true,
            Some((old, old_state)) => {
                let old_anchor = old_state.anchor.expect("eligible fork root");
                let weight = candidate.total_weight >= old.total_weight;
                (weight && anchor.total_weight > old_anchor.total_weight)
                    || ((weight || anchor.total_weight >= old_anchor.total_weight)
                        && (state.total_votes > old_state.total_votes
                            || (state.total_votes == old_state.total_votes
                                && (candidate.height > old.height
                                    || (candidate.height == old.height
                                        && candidate.hash < old.hash)))))
            }
        };
        if replace {
            best = Some((candidate, state));
        }
    }
    out.best = best.map(|(candidate, _)| candidate.hash);
    out
}
