use std::sync::Arc;

use fred::mocks::MockCommand;
use fred::mocks::Mocks;
use fred::types::Value;
use parking_lot::RwLock;

// Mock the Redis connection to be able to simulate a timeout error coming from within
// the `fred` client
#[derive(Default, Debug, Clone)]
pub(crate) struct MockStorageTimeout(Arc<RwLock<Vec<MockCommand>>>);
impl Mocks for MockStorageTimeout {
    fn process_command(&self, command: MockCommand) -> Result<Value, fred::error::Error> {
        self.0.write().push(command);

        let timeout_error = fred::error::Error::new(fred::error::ErrorKind::Timeout, "");
        Err(timeout_error)
    }
}

impl MockStorageTimeout {
    pub(crate) fn commands(&self) -> Vec<MockCommand> {
        self.0.read().clone()
    }
}
