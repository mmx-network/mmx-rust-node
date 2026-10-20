use crate::{config, err, params, service, stopping, wallet_reward, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use mmx_timelord::{engine::Config, identity::Identity, Affinity, TimeLord};
use mmx_wallet::Address;
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};
use vnx::{
    net::{NetworkClient, ServerOptions},
    runtime::{Methods, OverflowPolicy, Runtime, Session},
    serde_json::{self, json},
    VnxType,
};

#[derive(Parser)]
#[command(
    name = "mmx-timelord",
    about = "Native MMX timelord with automatic efficiency-core placement"
)]
pub struct Args {
    #[command(flatten)]
    layers: config::Layers,
    /// Node VNX endpoint; native C++ nodes normally use port 11330.
    #[arg(short = 'n', long)]
    node: Option<String>,
    #[arg(long)]
    wallet: Option<String>,
    #[arg(long)]
    network: Option<String>,
    #[arg(long)]
    reward_addr: Option<Address>,
    /// Key directory; defaults to MMX_NETWORK, or MMX_HOME when unset.
    #[arg(long)]
    storage_path: Option<PathBuf>,
    #[arg(long, default_value_t = 1_000_000)]
    max_history: usize,
    /// auto selects an allowed SHA-capable Intel efficiency core; any disables affinity.
    #[arg(long, default_value = "auto")]
    cpu: String,
    #[arg(long, default_value = "127.0.0.1:11334")]
    listen: String,
    #[arg(long, default_value = "timelord.requests")]
    input_request: String,
    #[arg(long, default_value = "timelord.proof")]
    output_proofs: String,
    /// Hash this many iterations locally and exit without contacting a node.
    #[arg(long)]
    benchmark: Option<u64>,
}
fn affinity(value: &str) -> Result<Affinity> {
    match value {
        "auto" => Ok(Affinity::Auto),
        "any" => Ok(Affinity::Any),
        _ => Ok(Affinity::Core(value.parse()?)),
    }
}
fn benchmark(iterations: u64, cpu: Affinity) -> Result<()> {
    if iterations == 0 || iterations > u64::from(u32::MAX) {
        return Err(err("benchmark iterations must be 1..=4294967295"));
    }
    let mut config = Config::new(iterations as u32);
    config.max_history = 8;
    let mut timelord = TimeLord::spawn(config, cpu)?;
    let control = timelord.control();
    let start = Instant::now();
    control.submit(mmx_types::mmx::IntervalRequest {
        vdf_height: 1,
        start: 0,
        end: iterations,
        infuse: [2; 32].into(),
        input: Some([1; 32].into()),
    })?;
    let proof = loop {
        if let Some(p) = control.wait_completed(Duration::from_secs(1), 1)?.pop() {
            break p;
        }
        if start.elapsed() > Duration::from_secs(300) {
            return Err(err("timelord benchmark timed out"));
        }
    };
    let elapsed = start.elapsed();
    timelord.shutdown()?;
    println!(
        "{}",
        json!({"backend":format!("{:?}",timelord.backend()),"cpu":timelord.cpu(),"iterations":iterations,"elapsed_ms":elapsed.as_secs_f64()*1000.0,"mh_per_sec":iterations as f64/elapsed.as_secs_f64()/1e6,"output":vnx::util::to_hex_string(&proof.proof.segments.last().unwrap().0,false,false)})
    );
    Ok(())
}
pub fn timelord_main() -> Result<()> {
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    let cfg = args.layers.load()?;
    config::apply(&matches, &cfg, "node", "node", &mut args.node)?;
    config::apply(
        &matches,
        &cfg,
        "reward_addr",
        "TimeLord.reward_addr",
        &mut args.reward_addr,
    )?;
    config::apply(
        &matches,
        &cfg,
        "storage_path",
        "TimeLord.storage_path",
        &mut args.storage_path,
    )?;
    config::apply(
        &matches,
        &cfg,
        "max_history",
        "TimeLord.max_history",
        &mut args.max_history,
    )?;
    config::apply(&matches, &cfg, "cpu", "TimeLord.cpu", &mut args.cpu)?;
    config::apply(
        &matches,
        &cfg,
        "listen",
        "TimeLord.listen",
        &mut args.listen,
    )?;
    config::apply(
        &matches,
        &cfg,
        "input_request",
        "TimeLord.input_request",
        &mut args.input_request,
    )?;
    config::apply(
        &matches,
        &cfg,
        "output_proofs",
        "TimeLord.output_proofs",
        &mut args.output_proofs,
    )?;
    let cpu = affinity(&args.cpu)?;
    if let Some(iterations) = args.benchmark {
        return benchmark(iterations, cpu);
    }
    let address = config::remote_endpoint(args.node.as_deref().unwrap_or("127.0.0.1:11331"), 11330);
    let node = NetworkClient::connect(
        &address.parse()?,
        "Node",
        mmx_types::vnx_registry()?,
        Duration::from_secs(3),
    )?;
    let chain = params(&node, args.network.as_deref())?;
    let reward = if let Some(address) = args.reward_addr {
        address
    } else {
        let wallet_address = args
            .wallet
            .or(cfg.read("Wallet.listen")?)
            .unwrap_or_else(|| "127.0.0.1:11335".into());
        let mut reward = None;
        for address in [
            config::remote_endpoint(&wallet_address, 11335),
            address.clone(),
        ] {
            if let Ok(wallet) = NetworkClient::connect(
                &address.parse()?,
                "Wallet",
                mmx_types::vnx_registry()?,
                Duration::from_secs(1),
            ) {
                reward = wallet_reward(&wallet).ok().flatten();
                wallet.close();
                if reward.is_some() {
                    break;
                }
            }
        }
        reward.unwrap_or_default()
    };
    let mut config = Config::new(chain.vdf_segment_size);
    config.max_history = args.max_history;
    config.reward_addr = reward.0;
    let network_dir = std::env::var_os("MMX_NETWORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| args.layers.home());
    let storage = match args.storage_path {
        Some(path) if path.is_absolute() => path,
        Some(path) => network_dir.join(path),
        None => network_dir,
    };
    let identity = Identity::load_or_create(&storage.join("timelord_sk.dat"))?;
    println!(
        "Timelord key: {} / reward: {reward}",
        vnx::util::to_hex_string(&identity.public_key(), false, false)
    );
    let mut timelord = TimeLord::spawn(config, cpu)?;
    println!(
        "Timelord {:?}, CPU {}",
        timelord.backend(),
        timelord
            .cpu()
            .map_or_else(|| "scheduler".into(), |v| v.to_string())
    );
    let control = timelord.control();
    let runtime = Runtime::new()?;
    let mut methods = Methods::new();
    let rpc = control.clone();
    methods.add(
        "mmx.TimeLord.stop_vdf",
        Some("mmx.permission_e.REMOTE".into()),
        move |_, _| {
            rpc.stop().map_err(|e| vnx::Error::Remote(e.to_string()))?;
            Ok(json!({}))
        },
    );
    runtime.register("TimeLord", methods)?;
    let mut server = service::bind_private(
        args.listen.parse()?,
        runtime,
        ServerOptions {
            services: ["TimeLord".into()].into(),
            session: Session::new("local", ["mmx.permission_e.REMOTE".into()]),
            ..Default::default()
        },
    )?;
    println!("TimeLord VNX RPC {}", server.endpoint());
    let requests = node.subscribe(&args.input_request, 256, OverflowPolicy::DropOldest, false)?;
    let stop = stopping()?;
    let result = thread::scope(|scope| {
        let request_stop = stop.clone();
        let input = control.clone();
        let reader = scope.spawn(move || -> Result<()> {
            while !request_stop.load(Ordering::Relaxed) {
                match requests.recv_timeout(Duration::from_millis(100)) {
                    Ok(sample) if !sample.replay => {
                        let request = (|| -> Result<()> {
                            if sample.value["__type"] != "mmx.IntervalRequest" {
                                return Err(err("expected mmx.IntervalRequest"));
                            }
                            input.submit(serde_json::from_value(sample.value)?)
                        })();
                        if let Err(e) = request {
                            eprintln!("Rejected timelord interval: {e}");
                        }
                    }
                    Ok(_) | Err(vnx::Error::Timeout) => (),
                    Err(_) if request_stop.load(Ordering::Relaxed) => break,
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        });
        let result = (|| -> Result<()> {
            let mut refresh = Instant::now();
            let mut report = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if reader.is_finished() {
                    break;
                }
                for completed in control.wait_completed(Duration::from_millis(100), 4)? {
                    let generation = completed.generation;
                    if !control.current(&completed) {
                        continue;
                    }
                    let proof = identity.sign(completed.proof)?;
                    if control.status()?.generation != generation {
                        continue;
                    }
                    if let Err(e) = node.publish(&args.output_proofs, proof.to_value()?) {
                        eprintln!("Timelord proof publication: {e}");
                    }
                }
                if refresh.elapsed() >= Duration::from_secs(2) {
                    control.refresh_publication()?;
                    refresh = Instant::now();
                }
                if report.elapsed() >= Duration::from_secs(10) {
                    let status = control.status()?;
                    if status.waiting {
                        println!(
                            "Timelord waiting for node intervals at {} iterations",
                            status.iterations
                        );
                    } else if status.running {
                        println!(
                            "Timelord {:.3} MH/s, iterations {}, {} history points, {} pending",
                            status.hashes_per_sec as f64 / 1e6,
                            status.iterations,
                            status.history,
                            status.pending
                        );
                    }
                    report = Instant::now();
                }
            }
            Ok(())
        })();
        stop.store(true, Ordering::Relaxed);
        node.close();
        let read = reader
            .join()
            .map_err(|_| err("timelord request worker panicked"))?;
        result?;
        read
    });
    server.shutdown();
    timelord.shutdown()?;
    result
}
