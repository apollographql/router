/// Functionality to run the `MONITOR` command against Redis, to ensure
/// specific commands are or are not sent.
use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinSet;

/// Represents a Redis command, which includes the actual command (ie GET) and any arguments (ie key1).
#[derive(Clone, Debug)]
struct Command {
    command: String,
    args: Vec<String>,
}

/// A `Monitor` manages two collections of tasks.
///
///  1) Each `monitor_task` runs the `MONITOR` command against a redis node and sends the commands
///     on a channel.
///  2) Each `collection_task` reads from a channel to collect the commands.
///
/// Running `monitor.collect()` will abort the first set of tasks and collect all remaining commands
/// in the channel, returning a `MonitorOutput`.
pub struct Monitor {
    monitor_tasks: JoinSet<()>,
    collection_tasks: JoinSet<SingleMonitorOutput>,
}

impl Monitor {
    /// Spawn the tasks to monitor each node in `ports`.
    pub async fn new(ports: &[&str]) -> Self {
        let mut monitor_tasks = JoinSet::new();
        let mut collection_tasks = JoinSet::new();

        for port in ports {
            let (tx, mut rx) = mpsc::channel(100);
            let port = port.to_string();

            monitor_tasks.spawn(async move {
                let is_replica = is_replica(&port).await;
                monitor(&port, is_replica, tx).await;
            });

            collection_tasks.spawn(async move {
                let mut commands = Vec::default();
                let mut is_replica = false;
                while let Some((is_rep, command)) = rx.recv().await {
                    commands.push(command);
                    is_replica = is_rep;
                }
                SingleMonitorOutput {
                    is_replica,
                    commands,
                }
            });
        }

        // sleep for a bit to allow tasks to spin up - do this here rather than requiring each
        // caller to do it
        tokio::time::sleep(Duration::from_secs(1)).await;

        Self {
            monitor_tasks,
            collection_tasks,
        }
    }

    /// End all `monitor_tasks` and collect the results into a `MonitorOutput`.
    pub async fn collect(mut self) -> MonitorOutput {
        // sleep a bit to make sure the monitor tasks have time to finish
        tokio::time::sleep(Duration::from_secs(1)).await;

        // abort monitor tasks and collect all the collection tasks
        self.monitor_tasks.abort_all();
        while self.monitor_tasks.join_next().await.is_some() {}

        let commands_results = self.collection_tasks.join_all().await;
        MonitorOutput(commands_results)
    }
}

/// The collected output from a `Monitor`.
///
/// The output can be filtered to:
/// * commands which apply to a specific namespace
/// * commands which were sent to either primaries or replicas
#[derive(Clone, Debug)]
pub struct MonitorOutput(Vec<SingleMonitorOutput>);
impl From<Vec<SingleMonitorOutput>> for MonitorOutput {
    fn from(value: Vec<SingleMonitorOutput>) -> Self {
        Self(value)
    }
}

impl MonitorOutput {
    pub fn namespaced(&self, namespace: &str) -> Self {
        let mut s = self.clone();
        for monitor_output in s.0.iter_mut() {
            monitor_output.commands.retain(|command| {
                command
                    .args
                    .first()
                    .is_some_and(|arg| arg.starts_with(namespace))
            });
        }
        s
    }

    pub fn replicas(&self, is_replica: bool) -> Self {
        let s = self.clone();
        Self(
            s.0.into_iter()
                .filter(|monitor_output| monitor_output.is_replica == is_replica)
                .collect(),
        )
    }

    pub fn command_sent_to_any(&self, cmd: &str) -> bool {
        self.0.iter().any(|output| output.command_sent(cmd))
    }

    pub fn command_sent_to_all(&self, cmd: &str) -> bool {
        self.0.iter().all(|output| output.command_sent(cmd))
    }

    pub fn command_sent_to_replicas_only(&self, cmd: &str) -> bool {
        let mget_sent_to_replica = self.replicas(true).command_sent_to_any(cmd);
        let mget_sent_to_primary = self.replicas(false).command_sent_to_any(cmd);
        mget_sent_to_replica && !mget_sent_to_primary
    }

    pub fn num_nodes(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug)]
struct SingleMonitorOutput {
    is_replica: bool,
    commands: Vec<Command>,
}

impl SingleMonitorOutput {
    fn command_sent(&self, cmd: &str) -> bool {
        self.commands.iter().any(|command| command.command == cmd)
    }
}

/// Determine whether the redis instance exposed at $port is a replica.
/// Returns `false` if this is a standalone instance, or if it's a primary node in a cluster.
// NB: this might be able to done with fred
async fn is_replica(port: &str) -> bool {
    let mut cmd = tokio::process::Command::new("redis-cli")
        .args(["-p", port, "INFO", "replication"])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to create redis-cli command for monitoring redis commands");

    let mut reader = BufReader::new(cmd.stdout.take().unwrap()).lines();
    let mut is_replica = None;
    while let Ok(Some(line)) = reader.next_line().await {
        if line.starts_with("role") {
            is_replica = Some(line.contains("role:slave"));
            break;
        }
    }

    is_replica.expect("no role information found")
}

/// Run the `MONITOR` command against a specific port and send the commands observed to a channel.
// NB: fred can't run MONITOR on a cluster, so we have to do it externally
async fn monitor(port: &str, is_replica: bool, tx: Sender<(bool, Command)>) {
    let mut cmd = tokio::process::Command::new("redis-cli")
        .args(["-p", port, "MONITOR"])
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to create redis-cli command for monitoring redis commands");

    let mut reader = BufReader::new(cmd.stdout.take().unwrap()).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if let Some(redis_command) = parse_monitor_output(&line) {
            tx.send((is_replica, redis_command))
                .await
                .expect("unable to send");
        } else {
            match line.as_str() {
                "OK" => {}
                line => eprintln!("unable to parse line: {line}"),
            };
        }
    }
}

/// Attempts to turn a `MONITOR` output line into a `Command`.
///
/// Examples:
///   1762887162.148899 [0 172.21.0.1:56836] "keys" "*"
///   1762887173.366436 [0 172.21.0.1:56836] "get" "key1" "key2" "key3"
///   1762887180.129221 [0 172.21.0.1:56836] "ttl" "key3"
fn parse_monitor_output(line: &str) -> Option<Command> {
    // use two regexes - one to strip out the prefix of each line, and the other to get the command
    // and keys without quotation marks
    let re1 = Regex::new(r"^[0-9.]+ \[0 [0-9.:]+] (.*)$").ok()?;
    let re2 = Regex::new(r#""([^"]+)""#).ok()?;

    let cmd_and_args = re1.captures(line)?.get(1)?.as_str();

    let mut cmd = None;
    let mut args = Vec::default();
    for (_, [value]) in re2.captures_iter(cmd_and_args).map(|c| c.extract()) {
        if cmd.is_none() {
            cmd = Some(value.to_string());
        } else {
            args.push(value.to_string());
        }
    }

    Some(Command {
        command: cmd?,
        args,
    })
}
