use super::*;
use crate::{
    node::record,
    peer::{discovery_tests::node, respond, Client, Connection, Server},
    router::RouterConfig,
    Node,
};
use mmx_consensus::Value;
use rand::{RngCore, SeedableRng};
use std::{net::TcpListener, sync::mpsc};

fn pair(incoming: bool) -> (Client, Connection, Arc<Mutex<Node>>) {
    let local = node(21);
    let remote = node(22);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let outgoing = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let accepted = listener.accept().unwrap().0;
    let (socket, remote_socket) = if incoming {
        (accepted, outgoing)
    } else {
        (outgoing, accepted)
    };
    let (inbox, state, relay) = {
        let mut node = local.lock().unwrap();
        node.enable_background_transactions().unwrap();
        (node.inbox(), node.peer_state(), node.transaction_relay())
    };
    let connection = if incoming {
        Connection::incoming(socket, &state.router).unwrap()
    } else {
        Connection::new(socket).unwrap()
    };
    let permit = if incoming {
        state.connections.inbound(Some(100)).unwrap()
    } else {
        state.connections.outbound().unwrap()
    };
    let shared = local.clone();
    let worker = thread::spawn(move || {
        Client::from_connection(connection, shared, inbox, state, relay, None, permit).unwrap()
    });
    let mut remote_socket = Connection::new(remote_socket).unwrap();
    for _ in 0..5 {
        let request = receive(&mut remote_socket);
        respond(&remote, &mut remote_socket, &request).unwrap();
    }
    (worker.join().unwrap(), remote_socket, local)
}
fn receive(connection: &mut Connection) -> Record {
    connection.receive(Duration::from_secs(3)).unwrap().unwrap()
}
fn request(id: u64, method: Record) -> Record {
    record(
        "mmx.Request",
        [
            ("id".into(), Value::Uint(id)),
            ("method".into(), Value::Record(method)),
        ],
    )
}
fn large_reply() -> Record {
    let mut bytes = vec![0; 1024 * 1024];
    rand::rngs::StdRng::seed_from_u64(108).fill_bytes(&mut bytes);
    record(
        "mmx.Return",
        [
            ("id".into(), Value::Uint(900)),
            (
                "result".into(),
                Value::Record(record(
                    "mmx.Router.get_peers.return",
                    [("_ret_0".into(), Value::Sequence(vec![Value::String(bytes)]))],
                )),
            ),
        ],
    )
}
fn saturate(connection: &mut Connection, remote: &Connection) -> Record {
    socket2::SockRef::from(&connection.stream)
        .set_send_buffer_size(4096)
        .unwrap();
    socket2::SockRef::from(&remote.stream)
        .set_recv_buffer_size(32 * 1024)
        .unwrap();
    let payload = large_reply();
    connection.send(&payload).unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let blocked = connection
            .output
            .lock()
            .unwrap()
            .queue
            .as_ref()
            .unwrap()
            .blocked;
        if blocked {
            break;
        }
        assert!(Instant::now() < end, "test socket did not saturate");
        thread::sleep(Duration::from_millis(5));
    }
    payload
}

#[test]
fn stalled_writes_leave_receive_and_rejection_live_and_preserve_recovery_schemas() {
    let (mut client, mut remote, local) = pair(false);
    // One bounded batch may cross the native byte threshold. The next normal
    // send must reject before encoding any schemas, even with a tiny threshold.
    client
        .connection
        .output
        .lock()
        .unwrap()
        .queue
        .as_mut()
        .unwrap()
        .limit = 64 * 1024;
    let payload = saturate(&mut client.connection, &remote);
    let deferred = request(
        902,
        record(
            "mmx.Node.get_tx_ids_at",
            [("height".into(), Value::Uint(0))],
        ),
    );
    let failed = client.connection.send(&deferred).unwrap_err();
    assert_eq!(
        failed.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );

    // An unrelated authenticated connection can authenticate and exchange RPC
    // while this socket has an actual blocked write, not just a pending frame.
    let mut service = Server::bind("127.0.0.1:0", local.clone()).unwrap();
    let mut healthy = Client::connect(&service.address().to_string(), node(23)).unwrap();
    assert_eq!(healthy.height().unwrap(), 0);

    remote
        .send(&request(
            901,
            record(
                "mmx.Node.get_block",
                [("hash".into(), Value::Bytes(vec![0; 32]))],
            ),
        ))
        .unwrap();
    // The method would take the ledger mutex if dispatched. Always release it
    // and interrupt the owned socket before joining on a failed assertion.
    let held = local.lock().unwrap();
    let abort = client.connection.stream.try_clone().unwrap();
    let (finished, ready) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = client.poll();
        finished.send((client, result)).unwrap();
    });
    let result = ready.recv_timeout(Duration::from_millis(750));
    drop(held);
    if result.is_err() {
        let _ = abort.shutdown(Shutdown::Both);
    }
    worker.join().unwrap();
    let (mut client, result) = result.expect("receiver waited on socket output or ledger work");
    result.unwrap();
    assert_eq!(receive(&mut remote), payload);
    let rejected = receive(&mut remote);
    assert_eq!(rejected.field("id").unwrap().uint().unwrap(), 901);
    assert_eq!(
        rejected.field("result").unwrap().record().unwrap().name,
        "vnx.OverflowException"
    );
    let end = Instant::now() + Duration::from_secs(2);
    while client.connection.output.lock().unwrap().pressured() {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(5));
    }
    client.connection.send(&deferred).unwrap();
    assert_eq!(
        receive(&mut remote),
        deferred,
        "rejected encoding advanced stream schemas"
    );
    remote
        .send(&request(903, record("mmx.Node.get_height", [])))
        .unwrap();
    client.poll().unwrap();
    let reply = receive(&mut remote);
    assert_eq!(reply.field("id").unwrap().uint().unwrap(), 903);
    assert_eq!(
        reply
            .field("result")
            .unwrap()
            .record()
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .uint()
            .unwrap(),
        0
    );
    drop(healthy);
    service.shutdown();
}

#[test]
fn rejection_reserve_is_bounded_and_stalled_writer_cancellation_is_prompt() {
    let (mut client, mut remote, _) = pair(false);
    saturate(&mut client.connection, &remote);
    for id in 0..MAX_MESSAGES - 1 {
        client
            .connection
            .send(&request(
                1000 + id as u64,
                record("mmx.Node.get_height", []),
            ))
            .unwrap();
    }
    let full = client
        .connection
        .send(&request(2000, record("mmx.Node.get_height", [])))
        .unwrap_err();
    assert_eq!(
        full.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    for id in 0..REJECTION_MESSAGES {
        remote
            .send(&request(id as u64, record("mmx.Node.get_height", [])))
            .unwrap();
        client.poll().unwrap();
    }
    {
        let output = client.connection.output.lock().unwrap();
        let q = output.queue.as_ref().unwrap();
        assert_eq!(q.rejections, REJECTION_MESSAGES);
        assert!(q.rejection_bytes <= REJECTION_BYTES);
        assert!(q.bytes <= q.limit + MAX_FRAME + 8);
        assert_eq!(q.messages, MAX_MESSAGES);
    }
    remote
        .send(&request(500, record("mmx.Node.get_height", [])))
        .unwrap();
    let failure = client.poll().unwrap_err();
    assert_eq!(
        failure.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let start = Instant::now();
    drop(client);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "shutdown waited for the socket write guard"
    );
}

#[test]
fn inbound_age_expires_a_blocked_writer_without_waiting_for_the_receiver() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut remote = Connection::connect(&listener.local_addr().unwrap().to_string()).unwrap();
    let policy = RouterConfig {
        disconnect_interval: Some(Duration::from_millis(400)),
        send_interval: Duration::from_secs(60),
        ..Default::default()
    };
    let mut connection = Connection::incoming(listener.accept().unwrap().0, &policy).unwrap();
    connection
        .start_writer(policy.max_write_queue, policy.send_interval)
        .unwrap();
    saturate(&mut connection, &remote);
    let end = Instant::now() + Duration::from_secs(2);
    while !connection.writer.as_ref().unwrap().thread.is_finished() {
        // Receive activity cannot renew the inbound lifetime, and no receive
        // owner is required to notice expiry while its writer is backpressured.
        let _ = remote.send(&request(1, record("mmx.Node.get_height", [])));
        assert!(Instant::now() < end, "writer ignored inbound lifetime");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(connection
        .send(&request(2, record("mmx.Node.get_height", [])))
        .is_err());
}

#[test]
fn authenticated_inbound_peers_serve_rpc_but_cannot_qualify_for_historical_sync() {
    let (mut client, mut remote, _) = pair(true);
    let failure = client
        .qualify(client.peer_state.genesis(), client.peer_state.id())
        .unwrap_err();
    assert!(failure.to_string().contains("outbound connection"));
    remote
        .send(&request(1, record("mmx.Node.get_height", [])))
        .unwrap();
    client.poll().unwrap();
    let reply = receive(&mut remote);
    assert_eq!(
        reply.field("result").unwrap().record().unwrap().name,
        "mmx.Node.get_height.return"
    );
}

#[test]
fn failed_managed_batches_never_publish_prefixes_or_advance_the_schema_state() {
    let (mut client, mut remote, _) = pair(false);
    let valid = request(
        902,
        record(
            "mmx.Node.get_header",
            [("hash".into(), Value::Bytes(vec![0; 32]))],
        ),
    );
    assert!(client
        .connection
        .send_batch(&[valid.clone(), record("unknown.invalid_schema", [])])
        .is_err());
    client.connection.send(&valid).unwrap();
    assert_eq!(receive(&mut remote), valid);
    assert!(remote.receive(Duration::from_millis(30)).unwrap().is_none());
}

#[test]
fn partial_write_progress_does_not_renew_a_queued_batch_deadline() {
    use std::io::Read;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut remote = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let stream = listener.accept().unwrap().0;
    socket2::SockRef::from(&stream)
        .set_send_buffer_size(4096)
        .unwrap();
    socket2::SockRef::from(&remote)
        .set_recv_buffer_size(32 * 1024)
        .unwrap();
    stream.set_write_timeout(Some(WRITE_POLL)).unwrap();
    let mut output = Output::new(
        stream.try_clone().unwrap(),
        super::super::DEFAULT_COMPRESSED_LIMIT,
    );
    output.queue = Some(Queue::new(1024 * 1024));
    output.send_batch(&[large_reply()]).unwrap();
    let start = Instant::now();
    output
        .queue
        .as_mut()
        .unwrap()
        .pending
        .front_mut()
        .unwrap()
        .end = start + Duration::from_millis(200);
    let shared = Arc::new(Mutex::new(output));
    let writer = shared.clone();
    let abort = stream.try_clone().unwrap();
    let worker = thread::spawn(move || {
        let mut socket = Socket {
            stream,
            timeout: WRITE_POLL,
        };
        let result = drain(&writer, &mut socket, &AtomicBool::new(false), None);
        let _ = socket.stream.shutdown(Shutdown::Both);
        result
    });
    remote
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut received = 0;
    // Keep making some progress, substantially slower than one full batch.
    while start.elapsed() < Duration::from_millis(500) && !worker.is_finished() {
        if let Ok(count) = remote.read(&mut [0; 4096]) {
            received += count;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let timely = worker.is_finished();
    let _ = abort.shutdown(Shutdown::Both);
    let failure = worker.join().unwrap().unwrap_err();
    assert!(timely, "partial writes renewed the absolute batch deadline");
    assert_eq!(
        failure.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::TimedOut
    );
    assert!(received > 4096, "test did not make partial write progress");
}
