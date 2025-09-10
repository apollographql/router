use crate::{clients::Transaction, interfaces::ClientLike};

/// Functions that implement the [transactions](https://redis.io/commands#transactions) interface.
///
/// See the [Transaction](crate::clients::Transaction) client for more information;
#[cfg(feature = "transactions")]
#[cfg_attr(docsrs, doc(cfg(feature = "transactions")))]
pub trait TransactionInterface: ClientLike + Sized {
  /// Enter a MULTI block, executing subsequent commands as a transaction.
  ///
  /// <https://redis.io/commands/multi>
  fn multi(&self) -> Transaction {
    Transaction::from_inner(self.inner())
  }
}
