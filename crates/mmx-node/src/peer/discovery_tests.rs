use super::*;
use k256::ecdsa::SigningKey;
use mmx_execution::chain::{forks::ForkChain, genesis::Assets};
use mmx_types::VnxType;
use vnx::serde_json::{self, json, Value as Json};

pub(super) fn node(seed: u8) -> Arc<Mutex<Node>> {
    let genesis: Json = include_str!("../../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Json>(line).unwrap())
        .find(|row| row["name"] == "synthetic")
        .unwrap();
    let raw = |value: &Json| vnx::util::from_hex_string(value.as_str().unwrap()).unwrap();
    let mut params = mmx_types::mmx::ChainParams::decode(&raw(&genesis["params"])).unwrap();
    params.port = 11337;
    let assets = Assets {
        binaries: genesis["binaries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| Record::decode(&raw(value)).unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
        testnet_rewards: None,
        project_relay: None,
    };
    let mut chain = ForkChain::memory(params);
    chain.initialize(&assets).unwrap();
    let mut node = Node::from_chain(chain, SigningKey::from_slice(&[seed; 32]).unwrap()).unwrap();
    let config = vnx::config::Config::new();
    config.set("Router.discover_interval", json!(1)).unwrap();
    node.configure_router(&config).unwrap();
    node.peer_state().publish(0, true);
    Arc::new(Mutex::new(node))
}
fn reply(connection: &mut Connection, id: Value, addresses: Vec<Value>) {
    connection
        .send(&record(
            "mmx.Return",
            [
                ("id".into(), id),
                (
                    "result".into(),
                    Value::Record(record(
                        "mmx.Router.get_peers.return",
                        [("_ret_0".into(), Value::Sequence(addresses))],
                    )),
                ),
            ],
        ))
        .unwrap();
}
fn text(value: &str) -> Value {
    Value::String(value.as_bytes().to_vec())
}
fn request(connection: &mut Connection) -> Record {
    connection.receive(Duration::from_secs(3)).unwrap().unwrap()
}
fn pair(connection: &mut Connection) -> (Record, Record) {
    let first = request(connection);
    let second = request(connection);
    assert_eq!(
        first.field("method").unwrap().record().unwrap().name,
        "mmx.Router.get_peers"
    );
    assert_eq!(
        second.field("method").unwrap().record().unwrap().name,
        "mmx.Node.get_height"
    );
    (first, second)
}
#[test]
fn discovery_is_correlated_bounded_and_never_waited_on_by_an_unrelated_rpc() {
    use std::sync::mpsc::channel;
    let local = node(1);
    let book = local.lock().unwrap().peer_state().book;
    let remote = node(2);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (proceed, wait) = channel();
    let server = thread::spawn(move || {
        let mut connection = Connection::new(listener.accept().unwrap().0).unwrap();
        for _ in 0..5 {
            let auth = request(&mut connection);
            respond(&remote, &mut connection, &auth).unwrap();
        }
        let (discover, height) = pair(&mut connection);
        respond(&remote, &mut connection, &height).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut hints = vec![
            text("8.8.8.8"),
            text("127.0.0.1"),
            text("::ffff:127.0.0.1"),
            text("10.1.2.3"),
            text("peer.invalid"),
            Value::String(vec![255]),
            text("2606:4700::1111"),
        ];
        hints.resize(32, text("8.8.8.8"));
        hints.push(text("9.8.7.6"));
        reply(
            &mut connection,
            discover.field("id").unwrap().clone(),
            hints,
        );
        let (old, height) = pair(&mut connection);
        respond(&remote, &mut connection, &height).unwrap();
        // No second discovery while the first is unanswered.
        let height = request(&mut connection);
        assert_eq!(
            height.field("method").unwrap().record().unwrap().name,
            "mmx.Node.get_height"
        );
        respond(&remote, &mut connection, &height).unwrap();
        let (current, height) = pair(&mut connection);
        reply(
            &mut connection,
            old.field("id").unwrap().clone(),
            vec![text("9.9.9.9")],
        );
        reply(&mut connection, Value::Uint(999999), vec![text("4.2.2.2")]);
        reply(
            &mut connection,
            current.field("id").unwrap().clone(),
            vec![text("1.1.1.1"), text("not-an-ip")],
        );
        respond(&remote, &mut connection, &height).unwrap();
        let (optional, height) = pair(&mut connection);
        connection
            .send(&record(
                "mmx.Return",
                [
                    ("id".into(), optional.field("id").unwrap().clone()),
                    (
                        "result".into(),
                        Value::Record(
                            Record::from_generated(&mmx_types::vnx::OverflowException::default())
                                .unwrap(),
                        ),
                    ),
                ],
            ))
            .unwrap();
        respond(&remote, &mut connection, &height).unwrap();
    });
    let mut client = Client::connect(&address.to_string(), local).unwrap();
    client.enable_discovery(true);
    assert_eq!(client.height().unwrap(), 0);
    proceed.send(()).unwrap();
    client.poll().unwrap();
    assert_eq!(
        book.candidates(0).unwrap().unwrap().1.len(),
        2,
        "only bounded, routable numeric hints are retained"
    );
    assert!(book.verified().unwrap().is_empty());
    client.discovery_next = Instant::now();
    assert_eq!(client.height().unwrap(), 0);
    client.discovery_next = Instant::now();
    assert_eq!(client.height().unwrap(), 0);
    client.discovery_pending.as_mut().unwrap().1 = Instant::now();
    assert_eq!(client.height().unwrap(), 0);
    client.discovery_next = Instant::now();
    assert_eq!(client.height().unwrap(), 0);
    assert_eq!(client.backpressure_retries(), 0);
    server.join().unwrap();
    let candidates = book.candidates(0).unwrap().unwrap().1;
    assert_eq!(candidates.len(), 3);
    assert!(candidates
        .iter()
        .any(|address| address.starts_with("1.1.1.1:")));
    assert!(!candidates
        .iter()
        .any(|address| address.starts_with("9.9.9.9:") || address.starts_with("4.2.2.2:")));
}
