use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

/// Items with higher priority value get handled sooner
#[allow(unused)]
#[derive(strum_macros::IntoStaticStr)]
pub(crate) enum Priority {
    P1 = 1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
    P8,
}

#[derive(Debug, Clone)]
pub(crate) enum SendError {
    QueueIsFull,
    Disconnected,
}

const INNER_QUEUES_COUNT: usize = Priority::P8 as usize - Priority::P1 as usize + 1;

/// Indices start at 0 for highest priority
const fn index_from_priority(priority: Priority) -> usize {
    Priority::P8 as usize - priority as usize
}

/// Indices start at 0 for highest priority
const fn priority_from_index(idx: usize) -> Priority {
    match idx {
        0 => Priority::P8,
        1 => Priority::P7,
        2 => Priority::P6,
        3 => Priority::P5,
        4 => Priority::P4,
        5 => Priority::P3,
        6 => Priority::P2,
        7 => Priority::P1,
        _ => {
            panic!("invalid index")
        }
    }
}

const _: () = {
    assert!(index_from_priority(Priority::P1) == 7);
    assert!(index_from_priority(Priority::P8) == 0);
    assert!(index_from_priority(priority_from_index(7)) == 7);
    assert!(index_from_priority(priority_from_index(0)) == 0);
};

pub(crate) struct AgeingPriorityQueue<T>
where
    T: Send + 'static,
{
    /// Items in **lower** indices queues are handled sooner
    inner_queues:
        [(crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>); INNER_QUEUES_COUNT],
    pub(crate) queued_count: AtomicUsize,
    capacity: usize,
}

pub(crate) struct Receiver<'a, T>
where
    T: Send + 'static,
{
    shared: &'a AgeingPriorityQueue<T>,
    select: crossbeam_channel::Select<'a>,
}

impl<T> AgeingPriorityQueue<T>
where
    T: Send + 'static,
{
    pub(crate) fn bounded(capacity: usize) -> Self {
        Self {
            // Using unbounded channels: callers must use `is_full` to implement backpressure
            inner_queues: std::array::from_fn(|_| crossbeam_channel::unbounded()),
            queued_count: AtomicUsize::new(0),
            capacity,
        }
    }

    pub(crate) fn queued_count(&self) -> usize {
        self.queued_count.load(Ordering::Relaxed)
    }

    /// Panics if `priority` is not in `AVAILABLE_PRIORITIES`
    pub(crate) fn send(&self, priority: Priority, message: T) -> Result<(), SendError> {
        self.queued_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous_count| {
                (previous_count < self.capacity).then_some(previous_count + 1)
            })
            .map_err(|_| SendError::QueueIsFull)?;
        let (inner_sender, _) = &self.inner_queues[index_from_priority(priority)];
        inner_sender
            .send(message)
            .map_err(|crossbeam_channel::SendError(_)| SendError::Disconnected)
    }

    pub(crate) fn receiver(&self) -> Receiver<'_, T> {
        let mut select = crossbeam_channel::Select::new_biased();
        for (_, inner_receiver) in &self.inner_queues {
            select.recv(inner_receiver);
        }
        Receiver {
            shared: self,
            select,
        }
    }
}

impl<T> Receiver<'_, T>
where
    T: Send + 'static,
{
    pub(crate) fn blocking_recv(&mut self) -> (T, Priority) {
        // Because we used `Select::new_biased` above,
        // `select()` will not shuffle receivers as it would with `Select::new` (for fairness)
        // but instead will try each one in priority order.
        let selected = self.select.select();
        let index = selected.index();
        let (_tx, rx) = &self.shared.inner_queues[index];
        // This `expect` can never panic because this channel can never be disconnected
        // because its sender is right here in `_tx`.
        let item = selected.recv(rx).expect("disconnected channel");
        self.shared.queued_count.fetch_sub(1, Ordering::Relaxed);
        self.age(index);
        (item, priority_from_index(index))
    }

    // Promote some messages from priorities lower (higher indices) than `message_consumed_at_index`
    fn age(&self, message_consumed_at_index: usize) {
        for window in self.shared.inner_queues[message_consumed_at_index..].windows(2) {
            let [higher_priority, lower_priority] = window else {
                panic!("expected windows of length 2")
            };
            let (higher_priority_sender, _higher_priority_receiver) = higher_priority;
            let (_, lower_priority_receiver) = lower_priority;
            if let Ok(message) = lower_priority_receiver.try_recv() {
                // This `expect` can never panic because this channel can never be disconnected
                // because its sender is right here in `_higher_priority_receiver`.
                higher_priority_sender
                    .send(message)
                    .expect("disconnected channel")
            }
        }
    }
}

#[test]
fn test_priorities() {
    let queue = AgeingPriorityQueue::bounded(4);
    assert_eq!(queue.queued_count(), 0);
    queue.send(Priority::P1, "p1").unwrap();
    assert_eq!(queue.queued_count(), 1);
    queue.send(Priority::P2, "p2").unwrap();
    queue.send(Priority::P3, "p3").unwrap();
    queue.send(Priority::P2, "p2 again").unwrap();
    assert_eq!(queue.queued_count(), 4);
    // The queue is full now, this send() fails:
    queue.send(Priority::P3, "p5").unwrap_err();
    assert_eq!(queue.queued_count(), 4);

    let mut receiver = queue.receiver();
    assert_eq!(receiver.blocking_recv().0, "p3");
    assert_eq!(receiver.blocking_recv().0, "p2");
    assert_eq!(receiver.blocking_recv().0, "p2 again");
    assert_eq!(receiver.blocking_recv().0, "p1");
    assert_eq!(queue.queued_count(), 0);
}
