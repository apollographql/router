use std::time::Duration;

struct Timeout {
    acquire: Duration,
    command: Duration,
    total: Duration,
}

impl Timeout {
    fn acquire(&self) -> Duration {
        self.acquire.min(self.total)
    }

    fn command(&self, acquire: Duration) -> Duration {
        let remaining = self.total - acquire;
        self.command.min(remaining)
    }
}
