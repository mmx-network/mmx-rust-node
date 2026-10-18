use mmx_consensus::{proof, Record};
use mmx_farming::{harvester::Harvester, plot::Prover};
use mmx_types::{mmx, VnxType};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use vnx::serde_json::{self, Value};
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures.json")).unwrap()
}
fn unhex(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}
fn bits(data: &mut [u8], offset: usize, count: usize, value: u64) {
    for i in 0..count {
        data[(offset + i) / 8] |= (((value >> i) & 1) as u8) << ((offset + i) % 8);
    }
}
fn make_plot(path: &Path, has_meta: bool, compressed: bool) -> mmx::PlotHeader {
    let f = fixture();
    let p = Record::decode(&unhex(&f["proof"]["wire"])).unwrap();
    let expected = &f["compute"]["entries"][0];
    assert_eq!(
        p.field("plot_id").unwrap().bytes().unwrap(),
        unhex(&f["compute"]["id"])
    );
    let xs: Vec<u32> = expected["proof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as u32)
        .collect();
    let mut h = mmx::PlotHeader {
        ksize: 8,
        xbits: if compressed { 7 } else { 8 },
        has_meta,
        seed: p
            .field("seed")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        plot_id: p
            .field("plot_id")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        farmer_key: p
            .field("farmer_key")
            .unwrap()
            .bytes()
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        park_size_x: 128,
        park_size_y: 1,
        park_size_pd: 128,
        park_size_meta: 1,
        park_bytes_x: 256,
        park_bytes_y: 4,
        park_bytes_pd: 160,
        park_bytes_meta: 12,
        entry_bits_x: 16,
        num_entries_y: 1,
        table_offset_y: 4096,
        table_offset_meta: 4100,
        table_offset_x: 4112,
        table_offset_pd: (0..7).map(|i| 4368 + i * 160).collect(),
        plot_size: 5488,
        ..Default::default()
    };
    if p.name == "mmx.ProofOfSpaceNFT" {
        h.contract = Some(vnx::Fixed(
            p.field("contract")
                .unwrap()
                .bytes()
                .unwrap()
                .try_into()
                .unwrap(),
        ));
    }
    let mut data = vec![0; h.plot_size as usize];
    let y = expected["y"].as_u64().unwrap();
    data[4096..4100].copy_from_slice(&(y as u32).to_le_bytes());
    let meta = unhex(&expected["meta"]);
    for (i, v) in meta.as_chunks::<4>().0.iter().enumerate() {
        bits(
            &mut data[4100..4112],
            i * 8,
            8,
            u32::from_le_bytes(*v).into(),
        );
    }
    for (i, pair) in xs.as_chunks::<2>().0.iter().enumerate() {
        let (mut a, mut b) = (
            pair[0] >> u32::from(compressed),
            pair[1] >> u32::from(compressed),
        );
        if a < b {
            std::mem::swap(&mut a, &mut b);
        }
        if compressed {
            a += 1;
            b += 1;
            if a == b {
                a += 1;
                b = 0;
            }
        }
        let index = u64::from(a) * u64::from(a - 1) / 2 + u64::from(b);
        bits(&mut data[4112..4368], i * 16, 16, index);
    }
    for base in &h.table_offset_pd {
        let park = &mut data[*base as usize..*base as usize + 160];
        for i in 0..128 {
            bits(park, i * 8, 8, 2 * i as u64);
            bits(park, 1024 + i * 2, 2, 1);
        }
    }
    let header = h.encode(&mmx_types::vnx_registry().unwrap()).unwrap();
    assert!(header.len() < 4096);
    data[..header.len()].copy_from_slice(&header);
    fs::write(path, data).unwrap();
    h
}
#[test]
fn native_plot_parks_recover_cpp_quality_and_canonical_proof_for_hdd_ssd_and_compression() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture();
    for (has_meta, compressed) in [(true, false), (false, false), (true, true)] {
        let path = dir.path().join(format!("{has_meta}-{compressed}.plot"));
        let h = make_plot(&path, has_meta, compressed);
        let p = Prover::open(&path).unwrap();
        let mut challenge = [0; 32];
        challenge[..4].copy_from_slice(
            &(f["compute"]["entries"][0]["y"].as_u64().unwrap() as u32).to_le_bytes(),
        );
        let qualities = p.qualities(&challenge, 0, 65536).unwrap();
        assert_eq!(qualities.len(), 1);
        assert_eq!(
            qualities[0].meta.as_slice(),
            unhex(&f["compute"]["entries"][0]["meta"])
        );
        let full = p.full_proof(0, 65536).unwrap();
        assert_eq!(full.meta, qualities[0].meta);
        assert_eq!(
            full.proof,
            f["compute"]["entries"][0]["proof"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
        assert!(p.full_proof(1, 65536).is_err());
        if compressed {
            assert!(p.full_proof(0, 1).is_err());
        }
        let mut challenge = [0; 32];
        challenge[0] = 255;
        assert!(p.qualities(&challenge, 0, 65536).unwrap().is_empty());
        // A changed plot identity must fail before any park lookup.
        let mut h = h;
        h.plot_id.0[0] ^= 1;
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&h.encode(&mmx_types::vnx_registry().unwrap()).unwrap())
            .unwrap();
        assert!(Prover::open(&path).is_err());
    }
}
#[test]
fn discovery_deduplicates_ids_rejects_unknown_keys_and_does_not_follow_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.plot");
    let h = make_plot(&path, true, false);
    fs::copy(&path, dir.path().join("two.plot")).unwrap();
    fs::write(dir.path().join("broken.plot"), b"bad").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();
    let mut harvester = Harvester::new("test".into(), [h.farmer_key.0]).unwrap();
    harvester.directories.insert(dir.path().to_path_buf());
    harvester.reload();
    assert_eq!(harvester.plots().len(), 1);
    assert_eq!(harvester.failures.len(), 2);
    let info = harvester.farm_info().unwrap();
    assert_eq!(info.total_bytes, 5488);
    assert_eq!(info.plot_count[&8], 1);
    assert_eq!(info.total_bytes_effective, 17 * 128 * 2464 / 1000);
    harvester.farmer_keys.clear();
    harvester.reload();
    assert!(harvester.plots().is_empty());
    // Truncation and oversized dimensions fail without large allocations.
    let mut h = h;
    h.park_bytes_x = i32::MAX;
    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&h.encode(&mmx_types::vnx_registry().unwrap()).unwrap())
        .unwrap();
    assert!(Prover::open(&path).is_err());
}
#[test]
fn harvested_response_is_accepted_by_the_cpp_derived_consensus_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.plot");
    let h = make_plot(&path, true, false);
    let f = fixture();
    let mut harvester = Harvester::new("test".into(), [h.farmer_key.0]).unwrap();
    harvester.directories.insert(dir.path().into());
    harvester.reload();
    let params = mmx::ChainParams::decode(&unhex(&f["proof"]["params"])).unwrap();
    let challenge = mmx::Challenge {
        vdf_height: f["proof"]["height"].as_u64().unwrap() as u32,
        challenge: vnx::Fixed(unhex(&f["proof"]["challenge"]).try_into().unwrap()),
        difficulty: 1,
    };
    let responses = harvester.lookup(&challenge, &params).unwrap();
    assert_eq!(responses.len(), 1);
    let p = responses[0].field("proof").unwrap().record().unwrap();
    proof::verify(p, &params, &challenge.challenge.0, 1, challenge.vdf_height).unwrap();
}

#[test]
fn cpp_written_and_cpp_proven_plot_files_are_read_without_conversion() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    for (mode, bytes) in [
        include_bytes!("native/native-0.plot").as_slice(),
        include_bytes!("native/native-1.plot").as_slice(),
        include_bytes!("native/native-2.plot").as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("native-{mode}.plot"));
        fs::write(&path, bytes).unwrap();
        let p = Prover::open(&path).unwrap();
        let full = p.full_proof(0, 65536).unwrap();
        assert_eq!(
            full.meta.as_slice(),
            unhex(&f["compute"]["entries"][0]["meta"])
        );
        assert_eq!(
            full.proof,
            f["compute"]["entries"][0]["proof"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
    }
}
