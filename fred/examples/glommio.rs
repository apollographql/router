use fred::prelude::*;
use futures::future::try_join_all;
use glommio::{prelude::*, DefaultStallDetectionHandler};
use log::info;
use std::{cell::RefCell, rc::Rc, time::SystemTime};

/// The number of threads in the Glommio pool builder.
const THREADS: usize = 8;
/// The total number of Redis clients used across all threads.
const POOL_SIZE: usize = 16;
/// The number of concurrent tasks spawned on each thread.
const CONCURRENCY: usize = 500;
/// The total number of increment commands sent to the servers.
const COUNT: usize = 100_000_000;

fn main() {
  pretty_env_logger::init();
  let config = Config::from_url("redis-cluster://foo:bar@redis-cluster-1:30001").unwrap();
  let builder = Builder::from_config(config);
  let started = SystemTime::now();

  LocalExecutorPoolBuilder::new(PoolPlacement::Unbound(THREADS))
    .on_all_shards(move || {
      // Each thread sends `COUNT / THREADS` commands to the server, sharing a client pool of `POOL_SIZE / THREADS`
      // clients among `CONCURRENCY` local tasks.
      let mut builder = builder.clone();
      let thread_id = executor().id();

      async move {
        // customize the task queues used by the client, if needed
        builder.with_connection_config(|config| {
          config.connection_task_queue =
            Some(executor().create_task_queue(Shares::default(), Latency::NotImportant, "connection_queue"));
          config.router_task_queue =
            Some(executor().create_task_queue(Shares::default(), Latency::NotImportant, "router_queue"));
        });

        let clients = POOL_SIZE / THREADS;
        let pool = builder.build_pool(clients)?;
        info!("{}: Connecting to Redis with {} clients", thread_id, clients);
        pool.init().await?;
        info!("{}: Starting incr loop", thread_id);
        incr_foo(&pool).await?;

        pool.quit().await?;
        Ok::<_, Error>(thread_id)
      }
    })
    .unwrap()
    .join_all()
    .into_iter()
    .for_each(|result| match result {
      Ok(Ok(id)) => println!("Finished thread {}", id),
      Ok(Err(e)) => println!("Redis error: {:?}", e),
      Err(e) => println!("Glommio error: {:?}", e),
    });

  let dur = SystemTime::now().duration_since(started).unwrap();
  let dur_sec = dur.as_secs() as f64 + (dur.subsec_millis() as f64 / 1000.0);
  println!(
    "Performed {} operations in: {:?}. Throughput: {} req/sec",
    COUNT,
    dur,
    (COUNT as f64 / dur_sec) as u64
  );
}

async fn incr_foo(pool: &Pool) -> Result<(), Error> {
  let counter = Rc::new(RefCell::new(0));
  let mut tasks = Vec::with_capacity(CONCURRENCY);
  for _ in 0 .. CONCURRENCY {
    let counter = counter.clone();
    let pool = pool.clone();
    tasks.push(spawn_local(async move {
      while *counter.borrow() < COUNT / THREADS {
        pool.incr::<(), _>("foo").await?;
        *counter.borrow_mut() += 1;
      }

      Ok::<_, Error>(())
    }));
  }
  try_join_all(tasks).await?;

  Ok(())
}
