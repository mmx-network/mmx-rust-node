//! Run twice with the same directory to demonstrate persistent contract state.
use mmx_vm::{
    instruction::CATCH_OVERFLOW, Engine, Instruction, Opcode, Storage, StorageDb, MEM_STATIC,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "./tmp/counter-db".into());
    let contract = [0x42; 32];
    let mut storage = StorageDb::open(path)?;
    let next_version = storage
        .database()
        .version()
        .checked_add(1)
        .ok_or("database version exhausted")?;
    let is_new = storage.read(&contract, MEM_STATIC, None)?.is_none();
    let (counter, gas) = {
        let mut engine = Engine::new(contract, &mut storage, false, 1, 100_000);
        engine.write(1, 1u64)?;
        if is_new {
            engine.write(MEM_STATIC, 0u64)?;
        }
        engine.init()?;
        let code = [
            Instruction::new(Opcode::Add, [MEM_STATIC as u32, MEM_STATIC as u32, 1, 0])
                .with_flags(CATCH_OVERFLOW),
            Instruction::new(Opcode::Ret, [0; 4]),
        ];
        engine.run(&code, 0)?;
        let counter = engine
            .read(MEM_STATIC)?
            .and_then(|value| value.as_uint())
            .ok_or("counter is not an integer")?;
        engine.commit()?;
        (counter, engine.gas_used)
    };
    storage.database_mut().commit(next_version)?;
    println!("version={next_version} counter={counter} gas={gas}");
    Ok(())
}
