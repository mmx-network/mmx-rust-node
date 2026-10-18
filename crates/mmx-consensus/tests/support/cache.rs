use mmx_consensus::{
    authorization::{validate_solution, AuthError},
    Record, Value,
};
use mmx_types::mmx;
use mmx_vm::{Engine, Instruction, Opcode, StorageDb, Value as VmValue, MEM_STACK};
fn bytes<const N: usize>(text: &str) -> [u8; N] {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
fn vm_verify(
    message: &[u8; 32],
    key: &[u8; 33],
    signature: &[u8; 64],
) -> Result<bool, mmx_vm::Error> {
    let mut storage = StorageDb::memory();
    let mut vm = Engine::new([1; 32], &mut storage, false, 1, 100_000);
    vm.write(1, VmValue::Binary(message.to_vec()))?;
    vm.write(2, VmValue::Binary(key.to_vec()))?;
    vm.write(3, VmValue::Binary(signature.to_vec()))?;
    vm.init()?;
    vm.run(
        &[
            Instruction::new(Opcode::Verify, [MEM_STACK as u32, 1, 2, 3]),
            Instruction::new(Opcode::Ret, [0; 4]),
        ],
        0,
    )?;
    Ok(vm.read(MEM_STACK)? == Some(VmValue::Bool(true)))
}
fn authorize(message: &[u8; 32], key: &[u8; 33], signature: &[u8; 64]) -> Result<(), AuthError> {
    let mut contract = Record::from_generated(&mmx::contract::PubKey::default()).unwrap();
    contract.fields.insert(
        "address".into(),
        Value::Bytes(mmx_crypto::public_key_address(key).to_vec()),
    );
    let mut solution = Record::from_generated(&mmx::solution::PubKey::default()).unwrap();
    solution
        .fields
        .insert("pubkey".into(), Value::Bytes(key.to_vec()));
    solution
        .fields
        .insert("signature".into(), Value::Bytes(signature.to_vec()));
    validate_solution(&contract, Some(&solution), message, 1)
}
/// Separate integration binaries give each direction the native fresh cache.
/// Changing either subsystem back to its own cache breaks these tests.
pub fn shared_cache(from_vm: bool) {
    assert!(vm_verify(&[0; 32], &[0; 33], &[0; 64]).unwrap());
    authorize(&[0; 32], &[0; 33], &[0; 64]).unwrap();
    let f: Vec<_> = include_str!("../../../mmx-crypto/tests/fixtures/native.tsv")
        .lines()
        .find(|s| s.starts_with("evict_zero\t"))
        .unwrap()
        .split('\t')
        .collect();
    let (message, key, signature) = (bytes(f[1]), bytes(f[2]), bytes(f[3]));
    if from_vm {
        assert!(vm_verify(&message, &key, &signature).unwrap());
        assert!(matches!(
            authorize(&[0; 32], &[0; 33], &[0; 64]),
            Err(AuthError::Crypto(
                mmx_crypto::VerifyError::PublicKeyEncoding
            ))
        ));
    } else {
        authorize(&message, &key, &signature).unwrap();
        assert!(matches!(
            vm_verify(&[0; 32], &[0; 33], &[0; 64]),
            Err(mmx_vm::Error::Signature)
        ));
    }
}
