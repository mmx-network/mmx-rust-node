use crate::{sha256, Error, Result};
use zeroize::Zeroizing;
/// MMX uses BIP39's English words, a reversed seed byte order, and accepts
/// both historic little-endian and current big-endian checksum variants.
pub fn seed_to_words(seed: &[u8; 32]) -> String {
    let words: Vec<_> = include_str!("english.txt").lines().collect();
    let mut big = Zeroizing::new(*seed);
    big.reverse();
    let checksum = sha256(big.as_slice())[0];
    // Sized for the longest words so the phrase is never reallocated (and
    // copied) while it is built.
    let mut phrase = String::with_capacity(24 * 9);
    for i in 0..24 {
        let mut index = 0usize;
        for j in 0..11 {
            let bit = i * 11 + j;
            index = (index << 1)
                | if bit < 256 {
                    ((big[bit / 8] >> (7 - bit % 8)) & 1) as usize
                } else {
                    ((checksum >> (7 - (bit - 256))) & 1) as usize
                };
        }
        if i > 0 {
            phrase.push(' ');
        }
        phrase.push_str(words[index]);
    }
    phrase
}
pub fn words_to_seed(phrase: &str) -> Result<[u8; 32]> {
    let words: Vec<_> = include_str!("english.txt").lines().collect();
    let input: Vec<_> = phrase.split_whitespace().collect();
    if input.len() != 24 {
        return Err(Error::InvalidMnemonic);
    }
    let mut big = Zeroizing::new([0u8; 32]);
    let mut checksum = 0u8;
    for (i, word) in input.iter().enumerate() {
        let index = words
            .iter()
            .position(|w| w == word)
            .ok_or(Error::InvalidMnemonic)?;
        for j in 0..11 {
            let bit = i * 11 + j;
            let v = ((index >> (10 - j)) & 1) as u8;
            if bit < 256 {
                big[bit / 8] |= v << (7 - bit % 8);
            } else {
                checksum = (checksum << 1) | v;
            }
        }
    }
    let current = sha256(big.as_slice())[0];
    big.reverse();
    if current != checksum && sha256(big.as_slice())[0] != checksum {
        return Err(Error::InvalidMnemonic);
    }
    // "abandon … art" has a valid checksum but encodes the all-zero seed,
    // which no wallet can use (key derivation and key files reject it).
    if *big == [0; 32] {
        return Err(Error::InvalidMnemonic);
    }
    Ok(*big)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_all_zero_phrase_is_rejected() {
        let zero = seed_to_words(&[0; 32]);
        assert!(zero.starts_with("abandon abandon"));
        assert!(matches!(words_to_seed(&zero), Err(Error::InvalidMnemonic)));
        let seed = [7u8; 32];
        assert_eq!(words_to_seed(&seed_to_words(&seed)).unwrap(), seed);
    }
}
