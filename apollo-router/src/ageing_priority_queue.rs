use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

/// Items with higher priority value get handled sooner
#[allow(unused)]
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

const INNER_QUEUES_COUNT: usize = Priority::P8 as usize - Priority::P1 as usize + 1;

/// Indices start at 0 for highest priority
const fn index_from_priority(priority: Priority) -> usize {
    Priority::P8 as usize - priority as usize
}

const _: () = {
    assert!(index_from_priority(Priority::P1) == 7);
    assert!(index_from_priority(Priority::P8) == 0);
};

pub(crate) struct AgeingPriorityQueue<T>
where
    T: Send + 'static,
{
    /// Items in **lower** indices queues are handled sooner
    inner_queues:
        [(crossbeam_channel::Sender<T>, crossbeam_channel::Receiver<T>); INNER_QUEUES_COUNT],
    queued_count: AtomicUsize,
    soft_capacity: usize,
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
    pub(crate) fn soft_bounded(soft_capacity: usize) -> Self {
        Self {
            // Using unbounded channels: callers must use `is_full` to implement backpressure
            inner_queues: std::array::from_fn(|_| crossbeam_channel::unbounded()),
            queued_count: AtomicUsize::new(0),
            soft_capacity,
        }
    }

    pub(crate) fn queued_count(&self) -> usize {
        self.queued_count.load(Ordering::Relaxed)
    }

    pub(crate) fn is_full(&self) -> bool {
        self.queued_count() >= self.soft_capacity
    }

    /// Panics if `priority` is not in `AVAILABLE_PRIORITIES`
    pub(crate) fn send(&self, priority: Priority, message: T) {
        self.queued_count.fetch_add(1, Ordering::Relaxed);
        let (inner_sender, _) = &self.inner_queues[index_from_priority(priority)];
        inner_sender.send(message).expect("disconnected channel")
    }

    pub(crate) fn receiver(&self) -> Receiver<'_, T> {
        let mut select = crossbeam_channel::Select::new();
        for (_, inner_receiver) in &self.inner_queues {
            select.recv(inner_receiver);
        }
        Receiver {
            shared: self,
            select,
        }
    }
}

impl<'a, T> Receiver<'a, T>
where
    T: Send + 'static,
{
    pub(crate) fn blocking_recv(&mut self) -> T {
        loop {
            // Block until something is ready.
            // Ignore the returned index because it is "random" when multiple operations are ready.
            self.select.ready();
            // Check inner channels in priority order instead:
            for (index, (_, inner_receiver)) in self.shared.inner_queues.iter().enumerate() {
                if let Ok(message) = inner_receiver.try_recv() {
                    self.shared.queued_count.fetch_sub(1, Ordering::Relaxed);
                    self.age(index);
                    return message;
                }
            }
            // Another thread raced us to it or `ready()` returned spuriously, try again
        }
    }

    // Promote some messages from priorities lower (higher indices) than `message_consumed_at_index`
    fn age(&self, message_consumed_at_index: usize) {
        for window in self.shared.inner_queues[message_consumed_at_index..].windows(2) {
            let [higher_priority, lower_priority] = window else {
                panic!("expected windows of length 2")
            };
            let (higher_priority_sender, _) = higher_priority;
            let (_, lower_priority_receiver) = lower_priority;
            if let Ok(message) = lower_priority_receiver.try_recv() {
                higher_priority_sender
                    .send(message)
                    .expect("disconnected channel")
            }
        }
    }
}
