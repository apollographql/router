//! In-process fake Redis deployments for testing how the client behaves on each topology without
//! Docker.
//!
//! Each deployment has one healthy primary and any number of replicas. Every replica is given a
//! [`Behaviour`], so tests can check what a single broken replica does to the pool. The fakes speak
//! just enough RESP2 for fred to connect, discover the topology the way it does against real Redis
//! (`ROLE` on a primary, `SENTINEL get-master-addr-by-name` on a sentinel, `CLUSTER SLOTS` on a
//! cluster node) and run the commands `RedisCacheStorage` sends. All healthy nodes share one
//! keyspace, standing in for instant replication.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::net::TcpSocket;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;

const HOST: &str = "127.0.0.1";
const SENTINEL_SERVICE_NAME: &str = "fake-primary";

/// How the router is told to reach the deployment.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Topology {
    /// A `redis://` URL to the primary; replicas are only reported by its `ROLE` reply.
    Centralized,
    /// A `redis-sentinel://` URL to a sentinel that reports the primary.
    Sentinel,
    /// A `redis-cluster://` URL to a primary that owns every hash slot, with its replicas listed
    /// in `CLUSTER SLOTS`.
    Cluster,
}

impl Topology {
    pub(crate) fn is_cluster(self) -> bool {
        matches!(self, Topology::Cluster)
    }
}

/// How a replica treats connections.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Behaviour {
    /// Serves commands like the primary.
    Healthy,
    /// Nothing listens on its port, so connections are refused (a stopped replica).
    Refusing,
    /// Accepts connections and closes them straight away (a crashing or restarting replica).
    Dropping,
    /// Accepts connections and never replies (a paused replica, or an HA standby that doesn't
    /// accept clients).
    Unresponsive,
}

/// What a node has seen, for asserting which nodes the client used.
#[derive(Default, Debug)]
pub(crate) struct NodeStats {
    connections: AtomicUsize,
    reads: AtomicUsize,
}

impl NodeStats {
    /// Connections accepted.
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// `GET` and `MGET` commands served.
    pub(crate) fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

pub(crate) struct Replica {
    pub(crate) port: u16,
    pub(crate) behaviour: Behaviour,
    pub(crate) stats: Arc<NodeStats>,
    /// Holds a refusing replica's port for the life of the deployment, so no other listener can
    /// take it.
    _reserved_port: Option<TcpSocket>,
}

pub(crate) struct FakeRedis {
    /// The URL to configure the router with.
    pub(crate) url: String,
    pub(crate) primary: Arc<NodeStats>,
    pub(crate) replicas: Vec<Replica>,
}

impl FakeRedis {
    /// Starts a deployment with a replica for each behaviour given.
    pub(crate) async fn start(topology: Topology, replica_behaviours: &[Behaviour]) -> Self {
        let keyspace = Keyspace::default();
        let primary_listener = bind();
        let primary_port = port(&primary_listener);

        let mut replicas = Vec::new();
        let mut replica_ports = Vec::new();
        for &behaviour in replica_behaviours {
            let socket = reserve();
            let replica_port = socket.local_addr().unwrap().port();
            replica_ports.push(replica_port);
            let (stats, reserved_port) = match behaviour {
                Behaviour::Healthy => {
                    let role = Role::Replica {
                        primary_port,
                        cluster: topology.is_cluster(),
                    };
                    (serve(listen(socket), role, keyspace.clone()), None)
                }
                // bound but not listening, so connections are refused
                Behaviour::Refusing => (Arc::default(), Some(socket)),
                Behaviour::Dropping => (drop_connections(listen(socket)), None),
                Behaviour::Unresponsive => (hold_connections(listen(socket)), None),
            };
            replicas.push(Replica {
                port: replica_port,
                behaviour,
                stats,
                _reserved_port: reserved_port,
            });
        }

        let primary_role = match topology {
            Topology::Cluster => Role::ClusterPrimary {
                port: primary_port,
                replica_ports,
            },
            Topology::Centralized | Topology::Sentinel => Role::Primary { replica_ports },
        };
        let primary = serve(primary_listener, primary_role, keyspace.clone());

        let url = match topology {
            Topology::Centralized => format!("redis://{HOST}:{primary_port}"),
            Topology::Cluster => format!("redis-cluster://{HOST}:{primary_port}"),
            Topology::Sentinel => {
                let listener = bind();
                let sentinel_port = port(&listener);
                serve(listener, Role::Sentinel { primary_port }, keyspace);
                format!(
                    "redis-sentinel://{HOST}:{sentinel_port}?sentinelServiceName={SENTINEL_SERVICE_NAME}"
                )
            }
        };

        Self {
            url,
            primary,
            replicas,
        }
    }

    /// Replicas that serve commands.
    pub(crate) fn healthy_replicas(&self) -> impl Iterator<Item = &NodeStats> {
        self.replicas
            .iter()
            .filter(|replica| matches!(replica.behaviour, Behaviour::Healthy))
            .map(|replica| replica.stats.as_ref())
    }

    /// Connections accepted across all replicas.
    pub(crate) fn replica_connections(&self) -> usize {
        self.replicas
            .iter()
            .map(|replica| replica.stats.connections())
            .sum()
    }
}

type Keyspace = Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>;

#[derive(Clone)]
enum Role {
    Primary { replica_ports: Vec<u16> },
    ClusterPrimary { port: u16, replica_ports: Vec<u16> },
    Replica { primary_port: u16, cluster: bool },
    Sentinel { primary_port: u16 },
}

/// Binds a free port without listening on it.
fn reserve() -> TcpSocket {
    let socket = TcpSocket::new_v4().unwrap();
    socket
        .bind((HOST.parse::<Ipv4Addr>().unwrap(), 0).into())
        .unwrap();
    socket
}

fn listen(socket: TcpSocket) -> TcpListener {
    socket.listen(1024).unwrap()
}

fn bind() -> TcpListener {
    listen(reserve())
}

fn port(listener: &TcpListener) -> u16 {
    listener.local_addr().unwrap().port()
}

fn serve(listener: TcpListener, role: Role, keyspace: Keyspace) -> Arc<NodeStats> {
    accept(listener, move |socket, stats| {
        tokio::spawn(serve_connection(
            socket,
            role.clone(),
            keyspace.clone(),
            stats,
        ));
    })
}

fn drop_connections(listener: TcpListener) -> Arc<NodeStats> {
    accept(listener, |socket, _| drop(socket))
}

fn hold_connections(listener: TcpListener) -> Arc<NodeStats> {
    let mut held = Vec::new();
    accept(listener, move |socket, _| held.push(socket))
}

/// Accepts connections until the test's runtime shuts down, counting each one.
fn accept(
    listener: TcpListener,
    mut on_connection: impl FnMut(TcpStream, Arc<NodeStats>) + Send + 'static,
) -> Arc<NodeStats> {
    let stats = Arc::new(NodeStats::default());
    let task_stats = stats.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            task_stats.connections.fetch_add(1, Ordering::SeqCst);
            on_connection(socket, task_stats.clone());
        }
    });
    stats
}

async fn serve_connection(
    socket: TcpStream,
    role: Role,
    keyspace: Keyspace,
    stats: Arc<NodeStats>,
) {
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    while let Some(args) = read_command(&mut reader).await {
        let reply = respond(&args, &role, &keyspace, &stats);
        if writer.write_all(&reply).await.is_err() {
            return;
        }
    }
}

/// Reads one command sent as a RESP array of bulk strings.
async fn read_command(reader: &mut BufReader<OwnedReadHalf>) -> Option<Vec<Vec<u8>>> {
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    let count: usize = line.trim().strip_prefix('*')?.parse().ok()?;
    let mut args = Vec::with_capacity(count);
    for _ in 0..count {
        line.clear();
        reader.read_line(&mut line).await.ok()?;
        let len: usize = line.trim().strip_prefix('$')?.parse().ok()?;
        let mut arg = vec![0; len + 2];
        reader.read_exact(&mut arg).await.ok()?;
        arg.truncate(len);
        args.push(arg);
    }
    Some(args)
}

fn respond(args: &[Vec<u8>], role: &Role, keyspace: &Keyspace, stats: &NodeStats) -> Vec<u8> {
    let word = |index: usize| {
        args.get(index)
            .map(|arg| String::from_utf8_lossy(arg).to_ascii_uppercase())
            .unwrap_or_default()
    };
    match (word(0).as_str(), role) {
        ("PING", _) => simple("PONG"),
        ("INFO", _) => bulk(b"# Server\r\nredis_version:7.4.0\r\n"),
        ("CLIENT", _) if word(1) == "ID" => integer(1),
        ("ROLE", Role::Primary { replica_ports }) => primary_role(replica_ports),
        ("ROLE", Role::Replica { primary_port, .. }) => replica_role(*primary_port),
        (
            "CLUSTER",
            Role::ClusterPrimary {
                port,
                replica_ports,
            },
        ) => match word(1).as_str() {
            "INFO" => bulk(b"cluster_state:ok\r\n"),
            "SLOTS" => cluster_slots(*port, replica_ports),
            other => error(&format!("unsupported CLUSTER {other}")),
        },
        ("CLUSTER", Role::Replica { cluster: true, .. }) if word(1) == "INFO" => {
            bulk(b"cluster_state:ok\r\n")
        }
        ("SENTINEL", Role::Sentinel { primary_port }) => match word(1).as_str() {
            "GET-MASTER-ADDR-BY-NAME" => array([bulk(HOST.as_bytes()), bulk_port(*primary_port)]),
            "SENTINELS" => array([]),
            other => error(&format!("unsupported SENTINEL {other}")),
        },
        ("GET", Role::Primary { .. } | Role::ClusterPrimary { .. } | Role::Replica { .. }) => {
            stats.reads.fetch_add(1, Ordering::SeqCst);
            nullable_bulk(keyspace.lock().get(&args[1]))
        }
        ("MGET", Role::Primary { .. } | Role::ClusterPrimary { .. } | Role::Replica { .. }) => {
            stats.reads.fetch_add(1, Ordering::SeqCst);
            let keyspace = keyspace.lock();
            array(args[1..].iter().map(|key| nullable_bulk(keyspace.get(key))))
        }
        ("SET", Role::Primary { .. } | Role::ClusterPrimary { .. }) => {
            keyspace.lock().insert(args[1].clone(), args[2].clone());
            simple("OK")
        }
        ("SET", Role::Replica { .. }) => {
            b"-READONLY You can't write against a read only replica.\r\n".to_vec()
        }
        // SELECT, CLIENT SETNAME, READONLY and anything else fred sends while setting up
        _ => simple("OK"),
    }
}

fn primary_role(replica_ports: &[u16]) -> Vec<u8> {
    let replicas = replica_ports
        .iter()
        .map(|&port| array([bulk(HOST.as_bytes()), bulk_port(port), bulk(b"0")]));
    array([bulk(b"master"), integer(0), array(replicas)])
}

fn replica_role(primary_port: u16) -> Vec<u8> {
    array([
        bulk(b"slave"),
        bulk(HOST.as_bytes()),
        integer(primary_port.into()),
        bulk(b"connected"),
        integer(0),
    ])
}

/// One slot range covering every slot, owned by the primary and served by all the replicas.
fn cluster_slots(primary_port: u16, replica_ports: &[u16]) -> Vec<u8> {
    let node = |port: u16| {
        array([
            bulk(HOST.as_bytes()),
            integer(port.into()),
            bulk(format!("node-{port}").as_bytes()),
        ])
    };
    let range = [integer(0), integer(16383), node(primary_port)]
        .into_iter()
        .chain(replica_ports.iter().map(|&port| node(port)));
    array([array(range)])
}

fn simple(value: &str) -> Vec<u8> {
    format!("+{value}\r\n").into_bytes()
}

fn error(message: &str) -> Vec<u8> {
    format!("-ERR {message}\r\n").into_bytes()
}

fn integer(value: i64) -> Vec<u8> {
    format!(":{value}\r\n").into_bytes()
}

fn bulk(value: &[u8]) -> Vec<u8> {
    let mut out = format!("${}\r\n", value.len()).into_bytes();
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
    out
}

fn bulk_port(port: u16) -> Vec<u8> {
    bulk(port.to_string().as_bytes())
}

fn nullable_bulk(value: Option<&Vec<u8>>) -> Vec<u8> {
    value.map_or_else(|| b"$-1\r\n".to_vec(), |value| bulk(value))
}

fn array(items: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let items: Vec<_> = items.into_iter().collect();
    let mut out = format!("*{}\r\n", items.len()).into_bytes();
    out.extend(items.into_iter().flatten());
    out
}
