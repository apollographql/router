use crate::{
  error::{Error, ErrorKind},
  interfaces::Resp3Frame,
  modules::inner::ClientInner,
  protocol::{
    hashers::ClusterHash,
    responders::ResponseKind,
    types::{ProtocolFrame, Server},
    utils as protocol_utils,
  },
  runtime::{AtomicBool, OneshotSender, RefCount},
  trace,
  types::{CustomCommand, Value},
  utils as client_utils,
  utils,
};
use bytes_utils::Str;
use redis_protocol::resp3::types::RespVersion;
use std::{
  convert::TryFrom,
  fmt,
  fmt::Formatter,
  mem,
  str,
  time::{Duration, Instant},
};

#[cfg(any(feature = "full-tracing", feature = "partial-tracing"))]
use crate::trace::CommandTraces;
#[cfg(feature = "mocks")]
use crate::{
  modules::mocks::MockCommand,
  protocol::types::ValueScanResult,
  runtime::Sender,
  types::scan::ScanResult,
  types::Key,
};

#[cfg(feature = "debug-ids")]
static COMMAND_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(feature = "debug-ids")]
pub fn command_counter() -> usize {
  COMMAND_COUNTER
    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    .saturating_add(1)
}

/// A channel for communication between connection reader tasks and futures returned to the caller.
pub type ResponseSender = OneshotSender<Result<Resp3Frame, Error>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterErrorKind {
  Moved,
  Ask,
}

impl fmt::Display for ClusterErrorKind {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      ClusterErrorKind::Moved => write!(f, "MOVED"),
      ClusterErrorKind::Ask => write!(f, "ASK"),
    }
  }
}

impl<'a> TryFrom<&'a str> for ClusterErrorKind {
  type Error = Error;

  fn try_from(value: &'a str) -> Result<Self, Self::Error> {
    match value {
      "MOVED" => Ok(ClusterErrorKind::Moved),
      "ASK" => Ok(ClusterErrorKind::Ask),
      _ => Err(Error::new(ErrorKind::Protocol, "Expected MOVED or ASK error.")),
    }
  }
}

// TODO organize these and gate them w/ the appropriate feature flags
#[derive(Clone, Eq, PartialEq)]
pub enum CommandKind {
  AclLoad,
  AclSave,
  AclList,
  AclUsers,
  AclGetUser,
  AclSetUser,
  AclDelUser,
  AclCat,
  AclGenPass,
  AclWhoAmI,
  AclLog,
  AclHelp,
  Append,
  Auth,
  Asking,
  BgreWriteAof,
  BgSave,
  BitCount,
  BitField,
  BitOp,
  BitPos,
  BlPop,
  BlMove,
  BrPop,
  BrPopLPush,
  BzPopMin,
  BzPopMax,
  BlmPop,
  BzmPop,
  ClientID,
  ClientInfo,
  ClientKill,
  ClientList,
  ClientGetName,
  ClientPause,
  ClientUnpause,
  ClientUnblock,
  ClientReply,
  ClientSetname,
  ClientGetRedir,
  ClientTracking,
  ClientTrackingInfo,
  ClientCaching,
  ClusterAddSlots,
  ClusterCountFailureReports,
  ClusterCountKeysInSlot,
  ClusterDelSlots,
  ClusterFailOver,
  ClusterForget,
  ClusterFlushSlots,
  ClusterGetKeysInSlot,
  ClusterInfo,
  ClusterKeySlot,
  ClusterMeet,
  ClusterMyID,
  ClusterNodes,
  ClusterReplicate,
  ClusterReset,
  ClusterSaveConfig,
  ClusterSetConfigEpoch,
  ClusterBumpEpoch,
  ClusterSetSlot,
  ClusterReplicas,
  ClusterSlots,
  ConfigGet,
  ConfigRewrite,
  ConfigSet,
  ConfigResetStat,
  Copy,
  DBSize,
  Decr,
  DecrBy,
  Del,
  Discard,
  Dump,
  Echo,
  Eval,
  EvalSha,
  Exec,
  Exists,
  Expire,
  ExpireAt,
  ExpireTime,
  Failover,
  FlushAll,
  FlushDB,
  GeoAdd,
  GeoHash,
  GeoPos,
  GeoDist,
  GeoRadius,
  GeoRadiusByMember,
  GeoSearch,
  GeoSearchStore,
  Get,
  GetBit,
  GetDel,
  GetRange,
  GetSet,
  HDel,
  HExists,
  HGet,
  HGetAll,
  HIncrBy,
  HIncrByFloat,
  HKeys,
  HLen,
  HMGet,
  HMSet,
  HSet,
  HSetNx,
  HStrLen,
  HVals,
  HRandField,
  HTtl,
  HExpire,
  HExpireAt,
  HExpireTime,
  HPTtl,
  HPExpire,
  HPExpireAt,
  HPExpireTime,
  HPersist,
  Incr,
  IncrBy,
  IncrByFloat,
  Info,
  Keys,
  LastSave,
  LIndex,
  LInsert,
  LLen,
  LMove,
  LPop,
  LPos,
  LPush,
  LPushX,
  LRange,
  LMPop,
  LRem,
  LSet,
  LTrim,
  Lcs,
  MemoryDoctor,
  MemoryHelp,
  MemoryMallocStats,
  MemoryPurge,
  MemoryStats,
  MemoryUsage,
  Mget,
  Migrate,
  Monitor,
  Move,
  Mset,
  Msetnx,
  Multi,
  Object,
  Persist,
  Pexpire,
  Pexpireat,
  PexpireTime,
  Pfadd,
  Pfcount,
  Pfmerge,
  Ping,
  Psetex,
  Pttl,
  Quit,
  Randomkey,
  Readonly,
  Readwrite,
  Rename,
  Renamenx,
  Restore,
  Role,
  Rpop,
  Rpoplpush,
  Rpush,
  Rpushx,
  Sadd,
  Save,
  Scard,
  Sdiff,
  Sdiffstore,
  Select,
  Sentinel,
  Set,
  Setbit,
  Setex,
  Setnx,
  Setrange,
  Shutdown,
  Sinter,
  Sinterstore,
  Sismember,
  Replicaof,
  Slowlog,
  Smembers,
  Smismember,
  Smove,
  Sort,
  SortRo,
  Spop,
  Srandmember,
  Srem,
  Strlen,
  Sunion,
  Sunionstore,
  Swapdb,
  Sync,
  Time,
  Touch,
  Ttl,
  Type,
  Unlink,
  Unwatch,
  Wait,
  Watch,
  // Streams
  XinfoConsumers,
  XinfoGroups,
  XinfoStream,
  Xadd,
  Xtrim,
  Xdel,
  Xrange,
  Xrevrange,
  Xlen,
  Xread,
  Xgroupcreate,
  XgroupCreateConsumer,
  XgroupDelConsumer,
  XgroupDestroy,
  XgroupSetId,
  Xreadgroup,
  Xack,
  Xclaim,
  Xautoclaim,
  Xpending,
  // Sorted Sets
  Zadd,
  Zcard,
  Zcount,
  Zdiff,
  Zdiffstore,
  Zincrby,
  Zinter,
  Zinterstore,
  Zlexcount,
  Zrandmember,
  Zrange,
  Zrangestore,
  Zrangebylex,
  Zrangebyscore,
  Zrank,
  Zrem,
  Zremrangebylex,
  Zremrangebyrank,
  Zremrangebyscore,
  Zrevrange,
  Zrevrangebylex,
  Zrevrangebyscore,
  Zrevrank,
  Zscore,
  Zmscore,
  Zunion,
  Zunionstore,
  Zpopmax,
  Zpopmin,
  Zmpop,
  // Scripts
  ScriptLoad,
  ScriptDebug,
  ScriptExists,
  ScriptFlush,
  ScriptKill,
  // Scanning
  Scan,
  Sscan,
  Hscan,
  Zscan,
  // Function
  Fcall,
  FcallRO,
  FunctionDelete,
  FunctionDump,
  FunctionFlush,
  FunctionKill,
  FunctionList,
  FunctionLoad,
  FunctionRestore,
  FunctionStats,
  // Pubsub
  Publish,
  PubsubChannels,
  PubsubNumpat,
  PubsubNumsub,
  PubsubShardchannels,
  PubsubShardnumsub,
  Spublish,
  Ssubscribe,
  Sunsubscribe,
  Unsubscribe,
  Subscribe,
  Psubscribe,
  Punsubscribe,
  // RedisJSON
  JsonArrAppend,
  JsonArrIndex,
  JsonArrInsert,
  JsonArrLen,
  JsonArrPop,
  JsonArrTrim,
  JsonClear,
  JsonDebugMemory,
  JsonDel,
  JsonGet,
  JsonMerge,
  JsonMGet,
  JsonMSet,
  JsonNumIncrBy,
  JsonObjKeys,
  JsonObjLen,
  JsonResp,
  JsonSet,
  JsonStrAppend,
  JsonStrLen,
  JsonToggle,
  JsonType,
  // Time Series
  TsAdd,
  TsAlter,
  TsCreate,
  TsCreateRule,
  TsDecrBy,
  TsDel,
  TsDeleteRule,
  TsGet,
  TsIncrBy,
  TsInfo,
  TsMAdd,
  TsMGet,
  TsMRange,
  TsMRevRange,
  TsQueryIndex,
  TsRange,
  TsRevRange,
  // RediSearch
  FtList,
  FtAggregate,
  FtSearch,
  FtCreate,
  FtAlter,
  FtAliasAdd,
  FtAliasDel,
  FtAliasUpdate,
  FtConfigGet,
  FtConfigSet,
  FtCursorDel,
  FtCursorRead,
  FtDictAdd,
  FtDictDel,
  FtDictDump,
  FtDropIndex,
  FtExplain,
  FtInfo,
  FtSpellCheck,
  FtSugAdd,
  FtSugDel,
  FtSugGet,
  FtSugLen,
  FtSynDump,
  FtSynUpdate,
  FtTagVals,
  // Commands with custom state or commands that don't map directly to the server's command interface.
  _Hello(RespVersion),
  _AuthAllCluster,
  _HelloAllCluster(RespVersion),
  _FlushAllCluster,
  _ScriptFlushCluster,
  _ScriptLoadCluster,
  _ScriptKillCluster,
  _FunctionLoadCluster,
  _FunctionFlushCluster,
  _FunctionDeleteCluster,
  _FunctionRestoreCluster,
  // When in RESP3 mode and **not** using the `bcast` arg then we send the command on all cluster node connections
  _ClientTrackingCluster,
  _Custom(CustomCommand),
}

impl fmt::Debug for CommandKind {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}", self.to_str_debug())
  }
}

impl CommandKind {
  pub fn is_scan(&self) -> bool {
    matches!(*self, CommandKind::Scan)
  }

  pub fn is_hscan(&self) -> bool {
    matches!(*self, CommandKind::Hscan)
  }

  pub fn is_sscan(&self) -> bool {
    matches!(*self, CommandKind::Sscan)
  }

  pub fn is_zscan(&self) -> bool {
    matches!(*self, CommandKind::Zscan)
  }

  pub fn is_hello(&self) -> bool {
    matches!(*self, CommandKind::_Hello(_) | CommandKind::_HelloAllCluster(_))
  }

  pub fn is_auth(&self) -> bool {
    matches!(*self, CommandKind::Auth)
  }

  pub fn is_value_scan(&self) -> bool {
    matches!(*self, CommandKind::Zscan | CommandKind::Hscan | CommandKind::Sscan)
  }

  pub fn is_multi(&self) -> bool {
    matches!(*self, CommandKind::Multi)
  }

  pub fn is_exec(&self) -> bool {
    matches!(*self, CommandKind::Exec)
  }

  pub fn is_discard(&self) -> bool {
    matches!(*self, CommandKind::Discard)
  }

  pub fn ends_transaction(&self) -> bool {
    matches!(*self, CommandKind::Exec | CommandKind::Discard)
  }

  pub fn is_mset(&self) -> bool {
    matches!(*self, CommandKind::Mset | CommandKind::Msetnx)
  }

  pub fn is_custom(&self) -> bool {
    matches!(*self, CommandKind::_Custom(_))
  }

  pub fn closes_connection(&self) -> bool {
    matches!(*self, CommandKind::Quit | CommandKind::Shutdown)
  }

  pub fn custom_hash_slot(&self) -> Option<u16> {
    match self {
      CommandKind::_Custom(ref cmd) => match cmd.cluster_hash {
        ClusterHash::Custom(ref val) => Some(*val),
        _ => None,
      },
      _ => None,
    }
  }

  /// Read the command's protocol string without panicking.
  ///
  /// Typically used for logging or debugging.
  pub fn to_str_debug(&self) -> &str {
    match *self {
      CommandKind::AclLoad => "ACL LOAD",
      CommandKind::AclSave => "ACL SAVE",
      CommandKind::AclList => "ACL LIST",
      CommandKind::AclUsers => "ACL USERS",
      CommandKind::AclGetUser => "ACL GETUSER",
      CommandKind::AclSetUser => "ACL SETUSER",
      CommandKind::AclDelUser => "ACL DELUSER",
      CommandKind::AclCat => "ACL CAT",
      CommandKind::AclGenPass => "ACL GENPASS",
      CommandKind::AclWhoAmI => "ACL WHOAMI",
      CommandKind::AclLog => "ACL LOG",
      CommandKind::AclHelp => "ACL HELP",
      CommandKind::Append => "APPEND",
      CommandKind::Auth => "AUTH",
      CommandKind::Asking => "ASKING",
      CommandKind::BgreWriteAof => "BGREWRITEAOF",
      CommandKind::BgSave => "BGSAVE",
      CommandKind::BitCount => "BITCOUNT",
      CommandKind::BitField => "BITFIELD",
      CommandKind::BitOp => "BITOP",
      CommandKind::BitPos => "BITPOS",
      CommandKind::BlPop => "BLPOP",
      CommandKind::BlMove => "BLMOVE",
      CommandKind::BrPop => "BRPOP",
      CommandKind::BzmPop => "BZMPOP",
      CommandKind::BlmPop => "BLMPOP",
      CommandKind::BrPopLPush => "BRPOPLPUSH",
      CommandKind::BzPopMin => "BZPOPMIN",
      CommandKind::BzPopMax => "BZPOPMAX",
      CommandKind::ClientID => "CLIENT ID",
      CommandKind::ClientInfo => "CLIENT INFO",
      CommandKind::ClientKill => "CLIENT KILL",
      CommandKind::ClientList => "CLIENT LIST",
      CommandKind::ClientGetName => "CLIENT GETNAME",
      CommandKind::ClientPause => "CLIENT PAUSE",
      CommandKind::ClientUnpause => "CLIENT UNPAUSE",
      CommandKind::ClientUnblock => "CLIENT UNBLOCK",
      CommandKind::ClientReply => "CLIENT REPLY",
      CommandKind::ClientSetname => "CLIENT SETNAME",
      CommandKind::ClientGetRedir => "CLIENT GETREDIR",
      CommandKind::ClientTracking => "CLIENT TRACKING",
      CommandKind::ClientTrackingInfo => "CLIENT TRACKINGINFO",
      CommandKind::ClientCaching => "CLIENT CACHING",
      CommandKind::ClusterAddSlots => "CLUSTER ADDSLOTS",
      CommandKind::ClusterCountFailureReports => "CLUSTER COUNT-FAILURE-REPORTS",
      CommandKind::ClusterCountKeysInSlot => "CLUSTER COUNTKEYSINSLOT",
      CommandKind::ClusterDelSlots => "CLUSTER DEL SLOTS",
      CommandKind::ClusterFailOver => "CLUSTER FAILOVER",
      CommandKind::ClusterForget => "CLUSTER FORGET",
      CommandKind::ClusterGetKeysInSlot => "CLUSTER GETKEYSINSLOTS",
      CommandKind::ClusterInfo => "CLUSTER INFO",
      CommandKind::ClusterKeySlot => "CLUSTER KEYSLOT",
      CommandKind::ClusterMeet => "CLUSTER MEET",
      CommandKind::ClusterNodes => "CLUSTER NODES",
      CommandKind::ClusterReplicate => "CLUSTER REPLICATE",
      CommandKind::ClusterReset => "CLUSTER RESET",
      CommandKind::ClusterSaveConfig => "CLUSTER SAVECONFIG",
      CommandKind::ClusterSetConfigEpoch => "CLUSTER SET-CONFIG-EPOCH",
      CommandKind::ClusterSetSlot => "CLUSTER SETSLOT",
      CommandKind::ClusterReplicas => "CLUSTER REPLICAS",
      CommandKind::ClusterSlots => "CLUSTER SLOTS",
      CommandKind::ClusterBumpEpoch => "CLUSTER BUMPEPOCH",
      CommandKind::ClusterFlushSlots => "CLUSTER FLUSHSLOTS",
      CommandKind::ClusterMyID => "CLUSTER MYID",
      CommandKind::ConfigGet => "CONFIG GET",
      CommandKind::ConfigRewrite => "CONFIG REWRITE",
      CommandKind::ConfigSet => "CONFIG SET",
      CommandKind::ConfigResetStat => "CONFIG RESETSTAT",
      CommandKind::Copy => "COPY",
      CommandKind::DBSize => "DBSIZE",
      CommandKind::Decr => "DECR",
      CommandKind::DecrBy => "DECRBY",
      CommandKind::Del => "DEL",
      CommandKind::Discard => "DISCARD",
      CommandKind::Dump => "DUMP",
      CommandKind::Echo => "ECHO",
      CommandKind::Eval => "EVAL",
      CommandKind::EvalSha => "EVALSHA",
      CommandKind::Exec => "EXEC",
      CommandKind::Exists => "EXISTS",
      CommandKind::Expire => "EXPIRE",
      CommandKind::ExpireAt => "EXPIREAT",
      CommandKind::ExpireTime => "EXPIRETIME",
      CommandKind::Failover => "FAILOVER",
      CommandKind::FlushAll => "FLUSHALL",
      CommandKind::FlushDB => "FLUSHDB",
      CommandKind::GeoAdd => "GEOADD",
      CommandKind::GeoHash => "GEOHASH",
      CommandKind::GeoPos => "GEOPOS",
      CommandKind::GeoDist => "GEODIST",
      CommandKind::GeoRadius => "GEORADIUS",
      CommandKind::GeoRadiusByMember => "GEORADIUSBYMEMBER",
      CommandKind::GeoSearch => "GEOSEARCH",
      CommandKind::GeoSearchStore => "GEOSEARCHSTORE",
      CommandKind::Get => "GET",
      CommandKind::GetDel => "GETDEL",
      CommandKind::GetBit => "GETBIT",
      CommandKind::GetRange => "GETRANGE",
      CommandKind::GetSet => "GETSET",
      CommandKind::HDel => "HDEL",
      CommandKind::_Hello(_) => "HELLO",
      CommandKind::HExists => "HEXISTS",
      CommandKind::HGet => "HGET",
      CommandKind::HGetAll => "HGETALL",
      CommandKind::HIncrBy => "HINCRBY",
      CommandKind::HIncrByFloat => "HINCRBYFLOAT",
      CommandKind::HKeys => "HKEYS",
      CommandKind::HLen => "HLEN",
      CommandKind::HMGet => "HMGET",
      CommandKind::HMSet => "HMSET",
      CommandKind::HSet => "HSET",
      CommandKind::HSetNx => "HSETNX",
      CommandKind::HStrLen => "HSTRLEN",
      CommandKind::HRandField => "HRANDFIELD",
      CommandKind::HTtl => "HTTL",
      CommandKind::HExpire => "HEXPIRE",
      CommandKind::HExpireAt => "HEXPIREAT",
      CommandKind::HExpireTime => "HEXPIRETIME",
      CommandKind::HPersist => "HPERSIST",
      CommandKind::HPTtl => "HPTTL",
      CommandKind::HPExpire => "HPEXPIRE",
      CommandKind::HPExpireAt => "HPEXPIREAT",
      CommandKind::HPExpireTime => "HPEXPIRETIME",
      CommandKind::HVals => "HVALS",
      CommandKind::Incr => "INCR",
      CommandKind::IncrBy => "INCRBY",
      CommandKind::IncrByFloat => "INCRBYFLOAT",
      CommandKind::Info => "INFO",
      CommandKind::Keys => "KEYS",
      CommandKind::LastSave => "LASTSAVE",
      CommandKind::LIndex => "LINDEX",
      CommandKind::LInsert => "LINSERT",
      CommandKind::LLen => "LLEN",
      CommandKind::LMove => "LMOVE",
      CommandKind::LPop => "LPOP",
      CommandKind::LPos => "LPOS",
      CommandKind::LPush => "LPUSH",
      CommandKind::LPushX => "LPUSHX",
      CommandKind::LRange => "LRANGE",
      CommandKind::LMPop => "LMPOP",
      CommandKind::LRem => "LREM",
      CommandKind::LSet => "LSET",
      CommandKind::LTrim => "LTRIM",
      CommandKind::Lcs => "LCS",
      CommandKind::MemoryDoctor => "MEMORY DOCTOR",
      CommandKind::MemoryHelp => "MEMORY HELP",
      CommandKind::MemoryMallocStats => "MEMORY MALLOC-STATS",
      CommandKind::MemoryPurge => "MEMORY PURGE",
      CommandKind::MemoryStats => "MEMORY STATS",
      CommandKind::MemoryUsage => "MEMORY USAGE",
      CommandKind::Mget => "MGET",
      CommandKind::Migrate => "MIGRATE",
      CommandKind::Monitor => "MONITOR",
      CommandKind::Move => "MOVE",
      CommandKind::Mset => "MSET",
      CommandKind::Msetnx => "MSETNX",
      CommandKind::Multi => "MULTI",
      CommandKind::Object => "OBJECT",
      CommandKind::Persist => "PERSIST",
      CommandKind::Pexpire => "PEXPIRE",
      CommandKind::Pexpireat => "PEXPIREAT",
      CommandKind::PexpireTime => "PEXPIRETIME",
      CommandKind::Pfadd => "PFADD",
      CommandKind::Pfcount => "PFCOUNT",
      CommandKind::Pfmerge => "PFMERGE",
      CommandKind::Ping => "PING",
      CommandKind::Psetex => "PSETEX",
      CommandKind::Psubscribe => "PSUBSCRIBE",
      CommandKind::Pttl => "PTTL",
      CommandKind::Publish => "PUBLISH",
      CommandKind::Punsubscribe => "PUNSUBSCRIBE",
      CommandKind::Quit => "QUIT",
      CommandKind::Randomkey => "RANDOMKEY",
      CommandKind::Readonly => "READONLY",
      CommandKind::Readwrite => "READWRITE",
      CommandKind::Rename => "RENAME",
      CommandKind::Renamenx => "RENAMENX",
      CommandKind::Restore => "RESTORE",
      CommandKind::Role => "ROLE",
      CommandKind::Rpop => "RPOP",
      CommandKind::Rpoplpush => "RPOPLPUSH",
      CommandKind::Rpush => "RPUSH",
      CommandKind::Rpushx => "RPUSHX",
      CommandKind::Sadd => "SADD",
      CommandKind::Save => "SAVE",
      CommandKind::Scard => "SCARD",
      CommandKind::Sdiff => "SDIFF",
      CommandKind::Sdiffstore => "SDIFFSTORE",
      CommandKind::Select => "SELECT",
      CommandKind::Sentinel => "SENTINEL",
      CommandKind::Set => "SET",
      CommandKind::Setbit => "SETBIT",
      CommandKind::Setex => "SETEX",
      CommandKind::Setnx => "SETNX",
      CommandKind::Setrange => "SETRANGE",
      CommandKind::Shutdown => "SHUTDOWN",
      CommandKind::Sinter => "SINTER",
      CommandKind::Sinterstore => "SINTERSTORE",
      CommandKind::Sismember => "SISMEMBER",
      CommandKind::Replicaof => "REPLICAOF",
      CommandKind::Slowlog => "SLOWLOG",
      CommandKind::Smembers => "SMEMBERS",
      CommandKind::Smismember => "SMISMEMBER",
      CommandKind::Smove => "SMOVE",
      CommandKind::Sort => "SORT",
      CommandKind::SortRo => "SORT_RO",
      CommandKind::Spop => "SPOP",
      CommandKind::Srandmember => "SRANDMEMBER",
      CommandKind::Srem => "SREM",
      CommandKind::Strlen => "STRLEN",
      CommandKind::Subscribe => "SUBSCRIBE",
      CommandKind::Sunion => "SUNION",
      CommandKind::Sunionstore => "SUNIONSTORE",
      CommandKind::Swapdb => "SWAPDB",
      CommandKind::Sync => "SYNC",
      CommandKind::Time => "TIME",
      CommandKind::Touch => "TOUCH",
      CommandKind::Ttl => "TTL",
      CommandKind::Type => "TYPE",
      CommandKind::Unsubscribe => "UNSUBSCRIBE",
      CommandKind::Unlink => "UNLINK",
      CommandKind::Unwatch => "UNWATCH",
      CommandKind::Wait => "WAIT",
      CommandKind::Watch => "WATCH",
      CommandKind::XinfoConsumers => "XINFO CONSUMERS",
      CommandKind::XinfoGroups => "XINFO GROUPS",
      CommandKind::XinfoStream => "XINFO STREAM",
      CommandKind::Xadd => "XADD",
      CommandKind::Xtrim => "XTRIM",
      CommandKind::Xdel => "XDEL",
      CommandKind::Xrange => "XRANGE",
      CommandKind::Xrevrange => "XREVRANGE",
      CommandKind::Xlen => "XLEN",
      CommandKind::Xread => "XREAD",
      CommandKind::Xgroupcreate => "XGROUP CREATE",
      CommandKind::XgroupCreateConsumer => "XGROUP CREATECONSUMER",
      CommandKind::XgroupDelConsumer => "XGROUP DELCONSUMER",
      CommandKind::XgroupDestroy => "XGROUP DESTROY",
      CommandKind::XgroupSetId => "XGROUP SETID",
      CommandKind::Xreadgroup => "XREADGROUP",
      CommandKind::Xack => "XACK",
      CommandKind::Xclaim => "XCLAIM",
      CommandKind::Xautoclaim => "XAUTOCLAIM",
      CommandKind::Xpending => "XPENDING",
      CommandKind::Zadd => "ZADD",
      CommandKind::Zcard => "ZCARD",
      CommandKind::Zcount => "ZCOUNT",
      CommandKind::Zdiff => "ZDIFF",
      CommandKind::Zdiffstore => "ZDIFFSTORE",
      CommandKind::Zincrby => "ZINCRBY",
      CommandKind::Zinter => "ZINTER",
      CommandKind::Zinterstore => "ZINTERSTORE",
      CommandKind::Zlexcount => "ZLEXCOUNT",
      CommandKind::Zrandmember => "ZRANDMEMBER",
      CommandKind::Zrange => "ZRANGE",
      CommandKind::Zrangestore => "ZRANGESTORE",
      CommandKind::Zrangebylex => "ZRANGEBYLEX",
      CommandKind::Zrangebyscore => "ZRANGEBYSCORE",
      CommandKind::Zrank => "ZRANK",
      CommandKind::Zrem => "ZREM",
      CommandKind::Zremrangebylex => "ZREMRANGEBYLEX",
      CommandKind::Zremrangebyrank => "ZREMRANGEBYRANK",
      CommandKind::Zremrangebyscore => "ZREMRANGEBYSCORE",
      CommandKind::Zrevrange => "ZREVRANGE",
      CommandKind::Zrevrangebylex => "ZREVRANGEBYLEX",
      CommandKind::Zrevrangebyscore => "ZREVRANGEBYSCORE",
      CommandKind::Zrevrank => "ZREVRANK",
      CommandKind::Zscore => "ZSCORE",
      CommandKind::Zmscore => "ZMSCORE",
      CommandKind::Zunion => "ZUNION",
      CommandKind::Zunionstore => "ZUNIONSTORE",
      CommandKind::Zpopmax => "ZPOPMAX",
      CommandKind::Zpopmin => "ZPOPMIN",
      CommandKind::Zmpop => "ZMPOP",
      CommandKind::Scan => "SCAN",
      CommandKind::Sscan => "SSCAN",
      CommandKind::Hscan => "HSCAN",
      CommandKind::Zscan => "ZSCAN",
      CommandKind::ScriptDebug => "SCRIPT DEBUG",
      CommandKind::ScriptExists => "SCRIPT EXISTS",
      CommandKind::ScriptFlush => "SCRIPT FLUSH",
      CommandKind::ScriptKill => "SCRIPT KILL",
      CommandKind::ScriptLoad => "SCRIPT LOAD",
      CommandKind::Spublish => "SPUBLISH",
      CommandKind::Ssubscribe => "SSUBSCRIBE",
      CommandKind::Sunsubscribe => "SUNSUBSCRIBE",
      CommandKind::_AuthAllCluster => "AUTH ALL CLUSTER",
      CommandKind::_HelloAllCluster(_) => "HELLO ALL CLUSTER",
      CommandKind::_FlushAllCluster => "FLUSHALL CLUSTER",
      CommandKind::_ScriptFlushCluster => "SCRIPT FLUSH CLUSTER",
      CommandKind::_ScriptLoadCluster => "SCRIPT LOAD CLUSTER",
      CommandKind::_ScriptKillCluster => "SCRIPT Kill CLUSTER",
      CommandKind::_FunctionLoadCluster => "FUNCTION LOAD CLUSTER",
      CommandKind::_FunctionFlushCluster => "FUNCTION FLUSH CLUSTER",
      CommandKind::_FunctionDeleteCluster => "FUNCTION DELETE CLUSTER",
      CommandKind::_FunctionRestoreCluster => "FUNCTION RESTORE CLUSTER",
      CommandKind::_ClientTrackingCluster => "CLIENT TRACKING CLUSTER",
      CommandKind::Fcall => "FCALL",
      CommandKind::FcallRO => "FCALL_RO",
      CommandKind::FunctionDelete => "FUNCTION DELETE",
      CommandKind::FunctionDump => "FUNCTION DUMP",
      CommandKind::FunctionFlush => "FUNCTION FLUSH",
      CommandKind::FunctionKill => "FUNCTION KILL",
      CommandKind::FunctionList => "FUNCTION LIST",
      CommandKind::FunctionLoad => "FUNCTION LOAD",
      CommandKind::FunctionRestore => "FUNCTION RESTORE",
      CommandKind::FunctionStats => "FUNCTION STATS",
      CommandKind::PubsubChannels => "PUBSUB CHANNELS",
      CommandKind::PubsubNumpat => "PUBSUB NUMPAT",
      CommandKind::PubsubNumsub => "PUBSUB NUMSUB",
      CommandKind::PubsubShardchannels => "PUBSUB SHARDCHANNELS",
      CommandKind::PubsubShardnumsub => "PUBSUB SHARDNUMSUB",
      CommandKind::JsonArrAppend => "JSON.ARRAPPEND",
      CommandKind::JsonArrIndex => "JSON.ARRINDEX",
      CommandKind::JsonArrInsert => "JSON.ARRINSERT",
      CommandKind::JsonArrLen => "JSON.ARRLEN",
      CommandKind::JsonArrPop => "JSON.ARRPOP",
      CommandKind::JsonArrTrim => "JSON.ARRTRIM",
      CommandKind::JsonClear => "JSON.CLEAR",
      CommandKind::JsonDebugMemory => "JSON.DEBUG MEMORY",
      CommandKind::JsonDel => "JSON.DEL",
      CommandKind::JsonGet => "JSON.GET",
      CommandKind::JsonMerge => "JSON.MERGE",
      CommandKind::JsonMGet => "JSON.MGET",
      CommandKind::JsonMSet => "JSON.MSET",
      CommandKind::JsonNumIncrBy => "JSON.NUMINCRBY",
      CommandKind::JsonObjKeys => "JSON.OBJKEYS",
      CommandKind::JsonObjLen => "JSON.OBJLEN",
      CommandKind::JsonResp => "JSON.RESP",
      CommandKind::JsonSet => "JSON.SET",
      CommandKind::JsonStrAppend => "JSON.STRAPPEND",
      CommandKind::JsonStrLen => "JSON.STRLEN",
      CommandKind::JsonToggle => "JSON.TOGGLE",
      CommandKind::JsonType => "JSON.TYPE",
      CommandKind::TsAdd => "TS.ADD",
      CommandKind::TsAlter => "TS.ALTER",
      CommandKind::TsCreate => "TS.CREATE",
      CommandKind::TsCreateRule => "TS.CREATERULE",
      CommandKind::TsDecrBy => "TS.DECRBY",
      CommandKind::TsDel => "TS.DEL",
      CommandKind::TsDeleteRule => "TS.DELETERULE",
      CommandKind::TsGet => "TS.GET",
      CommandKind::TsIncrBy => "TS.INCRBY",
      CommandKind::TsInfo => "TS.INFO",
      CommandKind::TsMAdd => "TS.MADD",
      CommandKind::TsMGet => "TS.MGET",
      CommandKind::TsMRange => "TS.MRANGE",
      CommandKind::TsMRevRange => "TS.MREVRANGE",
      CommandKind::TsQueryIndex => "TS.QUERYINDEX",
      CommandKind::TsRange => "TS.RANGE",
      CommandKind::TsRevRange => "TS.REVRANGE",
      CommandKind::FtList => "FT._LIST",
      CommandKind::FtAggregate => "FT.AGGREGATE",
      CommandKind::FtSearch => "FT.SEARCH",
      CommandKind::FtCreate => "FT.CREATE",
      CommandKind::FtAlter => "FT.ALTER",
      CommandKind::FtAliasAdd => "FT.ALIASADD",
      CommandKind::FtAliasDel => "FT.ALIASDEL",
      CommandKind::FtAliasUpdate => "FT.ALIASUPDATE",
      CommandKind::FtConfigGet => "FT.CONFIG GET",
      CommandKind::FtConfigSet => "FT.CONFIG SET",
      CommandKind::FtCursorDel => "FT.CURSOR DEL",
      CommandKind::FtCursorRead => "FT.CURSOR READ",
      CommandKind::FtDictAdd => "FT.DICTADD",
      CommandKind::FtDictDel => "FT.DICTDEL",
      CommandKind::FtDictDump => "FT.DICTDUMP",
      CommandKind::FtDropIndex => "FT.DROPINDEX",
      CommandKind::FtExplain => "FT.EXPLAIN",
      CommandKind::FtInfo => "FT.INFO",
      CommandKind::FtSpellCheck => "FT.SPELLCHECK",
      CommandKind::FtSugAdd => "FT.SUGADD",
      CommandKind::FtSugDel => "FT.SUGDEL",
      CommandKind::FtSugGet => "FT.SUGGET",
      CommandKind::FtSugLen => "FT.SUGLEN",
      CommandKind::FtSynDump => "FT.SYNDUMP",
      CommandKind::FtSynUpdate => "FT.SYNUPDATE",
      CommandKind::FtTagVals => "FT.TAGVALS",
      CommandKind::_Custom(ref kind) => &kind.cmd,
    }
  }

  /// Read the protocol string for a command, panicking for internal commands that don't map directly to redis
  /// command.
  pub(crate) fn cmd_str(&self) -> Str {
    let s = match *self {
      CommandKind::AclLoad
      | CommandKind::AclSave
      | CommandKind::AclList
      | CommandKind::AclUsers
      | CommandKind::AclGetUser
      | CommandKind::AclSetUser
      | CommandKind::AclDelUser
      | CommandKind::AclCat
      | CommandKind::AclGenPass
      | CommandKind::AclWhoAmI
      | CommandKind::AclLog
      | CommandKind::AclHelp => "ACL",
      CommandKind::Append => "APPEND",
      CommandKind::Auth => "AUTH",
      CommandKind::Asking => "ASKING",
      CommandKind::BgreWriteAof => "BGREWRITEAOF",
      CommandKind::BgSave => "BGSAVE",
      CommandKind::BitCount => "BITCOUNT",
      CommandKind::BitField => "BITFIELD",
      CommandKind::BitOp => "BITOP",
      CommandKind::BitPos => "BITPOS",
      CommandKind::BlPop => "BLPOP",
      CommandKind::BlMove => "BLMOVE",
      CommandKind::BrPop => "BRPOP",
      CommandKind::BrPopLPush => "BRPOPLPUSH",
      CommandKind::BzPopMin => "BZPOPMIN",
      CommandKind::BzPopMax => "BZPOPMAX",
      CommandKind::BzmPop => "BZMPOP",
      CommandKind::BlmPop => "BLMPOP",
      CommandKind::ClientID
      | CommandKind::ClientInfo
      | CommandKind::ClientKill
      | CommandKind::ClientList
      | CommandKind::ClientGetName
      | CommandKind::ClientPause
      | CommandKind::ClientUnpause
      | CommandKind::ClientUnblock
      | CommandKind::ClientReply
      | CommandKind::ClientSetname
      | CommandKind::ClientCaching
      | CommandKind::ClientTrackingInfo
      | CommandKind::ClientTracking
      | CommandKind::ClientGetRedir => "CLIENT",
      CommandKind::ClusterAddSlots
      | CommandKind::ClusterCountFailureReports
      | CommandKind::ClusterCountKeysInSlot
      | CommandKind::ClusterDelSlots
      | CommandKind::ClusterFailOver
      | CommandKind::ClusterForget
      | CommandKind::ClusterGetKeysInSlot
      | CommandKind::ClusterInfo
      | CommandKind::ClusterKeySlot
      | CommandKind::ClusterMeet
      | CommandKind::ClusterNodes
      | CommandKind::ClusterReplicate
      | CommandKind::ClusterReset
      | CommandKind::ClusterSaveConfig
      | CommandKind::ClusterSetConfigEpoch
      | CommandKind::ClusterSetSlot
      | CommandKind::ClusterReplicas
      | CommandKind::ClusterSlots
      | CommandKind::ClusterBumpEpoch
      | CommandKind::ClusterFlushSlots
      | CommandKind::ClusterMyID => "CLUSTER",
      CommandKind::ConfigGet | CommandKind::ConfigRewrite | CommandKind::ConfigSet | CommandKind::ConfigResetStat => {
        "CONFIG"
      },
      CommandKind::Copy => "COPY",
      CommandKind::DBSize => "DBSIZE",
      CommandKind::Decr => "DECR",
      CommandKind::DecrBy => "DECRBY",
      CommandKind::Del => "DEL",
      CommandKind::Discard => "DISCARD",
      CommandKind::Dump => "DUMP",
      CommandKind::Echo => "ECHO",
      CommandKind::Eval => "EVAL",
      CommandKind::EvalSha => "EVALSHA",
      CommandKind::Exec => "EXEC",
      CommandKind::Exists => "EXISTS",
      CommandKind::Expire => "EXPIRE",
      CommandKind::ExpireAt => "EXPIREAT",
      CommandKind::ExpireTime => "EXPIRETIME",
      CommandKind::Failover => "FAILOVER",
      CommandKind::FlushAll => "FLUSHALL",
      CommandKind::_FlushAllCluster => "FLUSHALL",
      CommandKind::FlushDB => "FLUSHDB",
      CommandKind::GeoAdd => "GEOADD",
      CommandKind::GeoHash => "GEOHASH",
      CommandKind::GeoPos => "GEOPOS",
      CommandKind::GeoDist => "GEODIST",
      CommandKind::GeoRadius => "GEORADIUS",
      CommandKind::GeoRadiusByMember => "GEORADIUSBYMEMBER",
      CommandKind::GeoSearch => "GEOSEARCH",
      CommandKind::GeoSearchStore => "GEOSEARCHSTORE",
      CommandKind::Get => "GET",
      CommandKind::GetDel => "GETDEL",
      CommandKind::GetBit => "GETBIT",
      CommandKind::GetRange => "GETRANGE",
      CommandKind::GetSet => "GETSET",
      CommandKind::HDel => "HDEL",
      CommandKind::_Hello(_) => "HELLO",
      CommandKind::HExists => "HEXISTS",
      CommandKind::HGet => "HGET",
      CommandKind::HGetAll => "HGETALL",
      CommandKind::HIncrBy => "HINCRBY",
      CommandKind::HIncrByFloat => "HINCRBYFLOAT",
      CommandKind::HKeys => "HKEYS",
      CommandKind::HLen => "HLEN",
      CommandKind::HMGet => "HMGET",
      CommandKind::HMSet => "HMSET",
      CommandKind::HSet => "HSET",
      CommandKind::HSetNx => "HSETNX",
      CommandKind::HStrLen => "HSTRLEN",
      CommandKind::HRandField => "HRANDFIELD",
      CommandKind::HTtl => "HTTL",
      CommandKind::HExpire => "HEXPIRE",
      CommandKind::HExpireAt => "HEXPIREAT",
      CommandKind::HExpireTime => "HEXPIRETIME",
      CommandKind::HPersist => "HPERSIST",
      CommandKind::HPTtl => "HPTTL",
      CommandKind::HPExpire => "HPEXPIRE",
      CommandKind::HPExpireAt => "HPEXPIREAT",
      CommandKind::HPExpireTime => "HPEXPIRETIME",
      CommandKind::HVals => "HVALS",
      CommandKind::Incr => "INCR",
      CommandKind::IncrBy => "INCRBY",
      CommandKind::IncrByFloat => "INCRBYFLOAT",
      CommandKind::Info => "INFO",
      CommandKind::Keys => "KEYS",
      CommandKind::LastSave => "LASTSAVE",
      CommandKind::LIndex => "LINDEX",
      CommandKind::LInsert => "LINSERT",
      CommandKind::LLen => "LLEN",
      CommandKind::LMove => "LMOVE",
      CommandKind::LPop => "LPOP",
      CommandKind::LPos => "LPOS",
      CommandKind::LPush => "LPUSH",
      CommandKind::LPushX => "LPUSHX",
      CommandKind::LRange => "LRANGE",
      CommandKind::LMPop => "LMPOP",
      CommandKind::LRem => "LREM",
      CommandKind::LSet => "LSET",
      CommandKind::LTrim => "LTRIM",
      CommandKind::Lcs => "LCS",
      CommandKind::MemoryDoctor => "MEMORY",
      CommandKind::MemoryHelp => "MEMORY",
      CommandKind::MemoryMallocStats => "MEMORY",
      CommandKind::MemoryPurge => "MEMORY",
      CommandKind::MemoryStats => "MEMORY",
      CommandKind::MemoryUsage => "MEMORY",
      CommandKind::Mget => "MGET",
      CommandKind::Migrate => "MIGRATE",
      CommandKind::Monitor => "MONITOR",
      CommandKind::Move => "MOVE",
      CommandKind::Mset => "MSET",
      CommandKind::Msetnx => "MSETNX",
      CommandKind::Multi => "MULTI",
      CommandKind::Object => "OBJECT",
      CommandKind::Persist => "PERSIST",
      CommandKind::Pexpire => "PEXPIRE",
      CommandKind::Pexpireat => "PEXPIREAT",
      CommandKind::PexpireTime => "PEXPIRETIME",
      CommandKind::Pfadd => "PFADD",
      CommandKind::Pfcount => "PFCOUNT",
      CommandKind::Pfmerge => "PFMERGE",
      CommandKind::Ping => "PING",
      CommandKind::Psetex => "PSETEX",
      CommandKind::Psubscribe => "PSUBSCRIBE",
      CommandKind::Pttl => "PTTL",
      CommandKind::Publish => "PUBLISH",
      CommandKind::Punsubscribe => "PUNSUBSCRIBE",
      CommandKind::Quit => "QUIT",
      CommandKind::Randomkey => "RANDOMKEY",
      CommandKind::Readonly => "READONLY",
      CommandKind::Readwrite => "READWRITE",
      CommandKind::Rename => "RENAME",
      CommandKind::Renamenx => "RENAMENX",
      CommandKind::Restore => "RESTORE",
      CommandKind::Role => "ROLE",
      CommandKind::Rpop => "RPOP",
      CommandKind::Rpoplpush => "RPOPLPUSH",
      CommandKind::Rpush => "RPUSH",
      CommandKind::Rpushx => "RPUSHX",
      CommandKind::Sadd => "SADD",
      CommandKind::Save => "SAVE",
      CommandKind::Scard => "SCARD",
      CommandKind::Sdiff => "SDIFF",
      CommandKind::Sdiffstore => "SDIFFSTORE",
      CommandKind::Select => "SELECT",
      CommandKind::Sentinel => "SENTINEL",
      CommandKind::Set => "SET",
      CommandKind::Setbit => "SETBIT",
      CommandKind::Setex => "SETEX",
      CommandKind::Setnx => "SETNX",
      CommandKind::Setrange => "SETRANGE",
      CommandKind::Shutdown => "SHUTDOWN",
      CommandKind::Sinter => "SINTER",
      CommandKind::Sinterstore => "SINTERSTORE",
      CommandKind::Sismember => "SISMEMBER",
      CommandKind::Replicaof => "REPLICAOF",
      CommandKind::Slowlog => "SLOWLOG",
      CommandKind::Smembers => "SMEMBERS",
      CommandKind::Smismember => "SMISMEMBER",
      CommandKind::Smove => "SMOVE",
      CommandKind::Sort => "SORT",
      CommandKind::SortRo => "SORT_RO",
      CommandKind::Spop => "SPOP",
      CommandKind::Srandmember => "SRANDMEMBER",
      CommandKind::Srem => "SREM",
      CommandKind::Strlen => "STRLEN",
      CommandKind::Subscribe => "SUBSCRIBE",
      CommandKind::Sunion => "SUNION",
      CommandKind::Sunionstore => "SUNIONSTORE",
      CommandKind::Swapdb => "SWAPDB",
      CommandKind::Sync => "SYNC",
      CommandKind::Time => "TIME",
      CommandKind::Touch => "TOUCH",
      CommandKind::Ttl => "TTL",
      CommandKind::Type => "TYPE",
      CommandKind::Unsubscribe => "UNSUBSCRIBE",
      CommandKind::Unlink => "UNLINK",
      CommandKind::Unwatch => "UNWATCH",
      CommandKind::Wait => "WAIT",
      CommandKind::Watch => "WATCH",
      CommandKind::XinfoConsumers | CommandKind::XinfoGroups | CommandKind::XinfoStream => "XINFO",
      CommandKind::Xadd => "XADD",
      CommandKind::Xtrim => "XTRIM",
      CommandKind::Xdel => "XDEL",
      CommandKind::Xrange => "XRANGE",
      CommandKind::Xrevrange => "XREVRANGE",
      CommandKind::Xlen => "XLEN",
      CommandKind::Xread => "XREAD",
      CommandKind::Xgroupcreate
      | CommandKind::XgroupCreateConsumer
      | CommandKind::XgroupDelConsumer
      | CommandKind::XgroupDestroy
      | CommandKind::XgroupSetId => "XGROUP",
      CommandKind::Xreadgroup => "XREADGROUP",
      CommandKind::Xack => "XACK",
      CommandKind::Xclaim => "XCLAIM",
      CommandKind::Xautoclaim => "XAUTOCLAIM",
      CommandKind::Xpending => "XPENDING",
      CommandKind::Zadd => "ZADD",
      CommandKind::Zcard => "ZCARD",
      CommandKind::Zcount => "ZCOUNT",
      CommandKind::Zdiff => "ZDIFF",
      CommandKind::Zdiffstore => "ZDIFFSTORE",
      CommandKind::Zincrby => "ZINCRBY",
      CommandKind::Zinter => "ZINTER",
      CommandKind::Zinterstore => "ZINTERSTORE",
      CommandKind::Zlexcount => "ZLEXCOUNT",
      CommandKind::Zrandmember => "ZRANDMEMBER",
      CommandKind::Zrange => "ZRANGE",
      CommandKind::Zrangestore => "ZRANGESTORE",
      CommandKind::Zrangebylex => "ZRANGEBYLEX",
      CommandKind::Zrangebyscore => "ZRANGEBYSCORE",
      CommandKind::Zrank => "ZRANK",
      CommandKind::Zrem => "ZREM",
      CommandKind::Zremrangebylex => "ZREMRANGEBYLEX",
      CommandKind::Zremrangebyrank => "ZREMRANGEBYRANK",
      CommandKind::Zremrangebyscore => "ZREMRANGEBYSCORE",
      CommandKind::Zrevrange => "ZREVRANGE",
      CommandKind::Zrevrangebylex => "ZREVRANGEBYLEX",
      CommandKind::Zrevrangebyscore => "ZREVRANGEBYSCORE",
      CommandKind::Zrevrank => "ZREVRANK",
      CommandKind::Zscore => "ZSCORE",
      CommandKind::Zmscore => "ZMSCORE",
      CommandKind::Zunion => "ZUNION",
      CommandKind::Zunionstore => "ZUNIONSTORE",
      CommandKind::Zpopmax => "ZPOPMAX",
      CommandKind::Zpopmin => "ZPOPMIN",
      CommandKind::Zmpop => "ZMPOP",
      CommandKind::ScriptDebug
      | CommandKind::ScriptExists
      | CommandKind::ScriptFlush
      | CommandKind::ScriptKill
      | CommandKind::ScriptLoad
      | CommandKind::_ScriptFlushCluster
      | CommandKind::_ScriptKillCluster
      | CommandKind::_ScriptLoadCluster => "SCRIPT",
      CommandKind::Spublish => "SPUBLISH",
      CommandKind::Ssubscribe => "SSUBSCRIBE",
      CommandKind::Sunsubscribe => "SUNSUBSCRIBE",
      CommandKind::Scan => "SCAN",
      CommandKind::Sscan => "SSCAN",
      CommandKind::Hscan => "HSCAN",
      CommandKind::Zscan => "ZSCAN",
      CommandKind::Fcall => "FCALL",
      CommandKind::FcallRO => "FCALL_RO",
      CommandKind::FunctionDelete
      | CommandKind::FunctionDump
      | CommandKind::FunctionFlush
      | CommandKind::FunctionKill
      | CommandKind::FunctionList
      | CommandKind::FunctionLoad
      | CommandKind::FunctionRestore
      | CommandKind::FunctionStats
      | CommandKind::_FunctionFlushCluster
      | CommandKind::_FunctionRestoreCluster
      | CommandKind::_FunctionDeleteCluster
      | CommandKind::_FunctionLoadCluster => "FUNCTION",
      CommandKind::PubsubChannels
      | CommandKind::PubsubNumpat
      | CommandKind::PubsubNumsub
      | CommandKind::PubsubShardchannels
      | CommandKind::PubsubShardnumsub => "PUBSUB",
      CommandKind::_AuthAllCluster => "AUTH",
      CommandKind::_HelloAllCluster(_) => "HELLO",
      CommandKind::_ClientTrackingCluster => "CLIENT",
      CommandKind::JsonArrAppend => "JSON.ARRAPPEND",
      CommandKind::JsonArrIndex => "JSON.ARRINDEX",
      CommandKind::JsonArrInsert => "JSON.ARRINSERT",
      CommandKind::JsonArrLen => "JSON.ARRLEN",
      CommandKind::JsonArrPop => "JSON.ARRPOP",
      CommandKind::JsonArrTrim => "JSON.ARRTRIM",
      CommandKind::JsonClear => "JSON.CLEAR",
      CommandKind::JsonDebugMemory => "JSON.DEBUG",
      CommandKind::JsonDel => "JSON.DEL",
      CommandKind::JsonGet => "JSON.GET",
      CommandKind::JsonMerge => "JSON.MERGE",
      CommandKind::JsonMGet => "JSON.MGET",
      CommandKind::JsonMSet => "JSON.MSET",
      CommandKind::JsonNumIncrBy => "JSON.NUMINCRBY",
      CommandKind::JsonObjKeys => "JSON.OBJKEYS",
      CommandKind::JsonObjLen => "JSON.OBJLEN",
      CommandKind::JsonResp => "JSON.RESP",
      CommandKind::JsonSet => "JSON.SET",
      CommandKind::JsonStrAppend => "JSON.STRAPPEND",
      CommandKind::JsonStrLen => "JSON.STRLEN",
      CommandKind::JsonToggle => "JSON.TOGGLE",
      CommandKind::JsonType => "JSON.TYPE",
      CommandKind::TsAdd => "TS.ADD",
      CommandKind::TsAlter => "TS.ALTER",
      CommandKind::TsCreate => "TS.CREATE",
      CommandKind::TsCreateRule => "TS.CREATERULE",
      CommandKind::TsDecrBy => "TS.DECRBY",
      CommandKind::TsDel => "TS.DEL",
      CommandKind::TsDeleteRule => "TS.DELETERULE",
      CommandKind::TsGet => "TS.GET",
      CommandKind::TsIncrBy => "TS.INCRBY",
      CommandKind::TsInfo => "TS.INFO",
      CommandKind::TsMAdd => "TS.MADD",
      CommandKind::TsMGet => "TS.MGET",
      CommandKind::TsMRange => "TS.MRANGE",
      CommandKind::TsMRevRange => "TS.MREVRANGE",
      CommandKind::TsQueryIndex => "TS.QUERYINDEX",
      CommandKind::TsRange => "TS.RANGE",
      CommandKind::TsRevRange => "TS.REVRANGE",
      CommandKind::FtList => "FT._LIST",
      CommandKind::FtAggregate => "FT.AGGREGATE",
      CommandKind::FtSearch => "FT.SEARCH",
      CommandKind::FtCreate => "FT.CREATE",
      CommandKind::FtAlter => "FT.ALTER",
      CommandKind::FtAliasAdd => "FT.ALIASADD",
      CommandKind::FtAliasDel => "FT.ALIASDEL",
      CommandKind::FtAliasUpdate => "FT.ALIASUPDATE",
      CommandKind::FtConfigGet => "FT.CONFIG",
      CommandKind::FtConfigSet => "FT.CONFIG",
      CommandKind::FtCursorDel => "FT.CURSOR",
      CommandKind::FtCursorRead => "FT.CURSOR",
      CommandKind::FtDictAdd => "FT.DICTADD",
      CommandKind::FtDictDel => "FT.DICTDEL",
      CommandKind::FtDictDump => "FT.DICTDUMP",
      CommandKind::FtDropIndex => "FT.DROPINDEX",
      CommandKind::FtExplain => "FT.EXPLAIN",
      CommandKind::FtInfo => "FT.INFO",
      CommandKind::FtSpellCheck => "FT.SPELLCHECK",
      CommandKind::FtSugAdd => "FT.SUGADD",
      CommandKind::FtSugDel => "FT.SUGDEL",
      CommandKind::FtSugGet => "FT.SUGGET",
      CommandKind::FtSugLen => "FT.SUGLEN",
      CommandKind::FtSynDump => "FT.SYNDUMP",
      CommandKind::FtSynUpdate => "FT.SYNUPDATE",
      CommandKind::FtTagVals => "FT.TAGVALS",
      CommandKind::_Custom(ref kind) => return kind.cmd.clone(),
    };

    client_utils::static_str(s)
  }

  /// Read the optional subcommand string for a command.
  pub fn subcommand_str(&self) -> Option<Str> {
    let s = match *self {
      CommandKind::ScriptDebug => "DEBUG",
      CommandKind::ScriptLoad => "LOAD",
      CommandKind::ScriptKill => "KILL",
      CommandKind::ScriptFlush => "FLUSH",
      CommandKind::ScriptExists => "EXISTS",
      CommandKind::_ScriptFlushCluster => "FLUSH",
      CommandKind::_ScriptLoadCluster => "LOAD",
      CommandKind::_ScriptKillCluster => "KILL",
      CommandKind::AclLoad => "LOAD",
      CommandKind::AclSave => "SAVE",
      CommandKind::AclList => "LIST",
      CommandKind::AclUsers => "USERS",
      CommandKind::AclGetUser => "GETUSER",
      CommandKind::AclSetUser => "SETUSER",
      CommandKind::AclDelUser => "DELUSER",
      CommandKind::AclCat => "CAT",
      CommandKind::AclGenPass => "GENPASS",
      CommandKind::AclWhoAmI => "WHOAMI",
      CommandKind::AclLog => "LOG",
      CommandKind::AclHelp => "HELP",
      CommandKind::ClusterAddSlots => "ADDSLOTS",
      CommandKind::ClusterCountFailureReports => "COUNT-FAILURE-REPORTS",
      CommandKind::ClusterCountKeysInSlot => "COUNTKEYSINSLOT",
      CommandKind::ClusterDelSlots => "DELSLOTS",
      CommandKind::ClusterFailOver => "FAILOVER",
      CommandKind::ClusterForget => "FORGET",
      CommandKind::ClusterGetKeysInSlot => "GETKEYSINSLOT",
      CommandKind::ClusterInfo => "INFO",
      CommandKind::ClusterKeySlot => "KEYSLOT",
      CommandKind::ClusterMeet => "MEET",
      CommandKind::ClusterNodes => "NODES",
      CommandKind::ClusterReplicate => "REPLICATE",
      CommandKind::ClusterReset => "RESET",
      CommandKind::ClusterSaveConfig => "SAVECONFIG",
      CommandKind::ClusterSetConfigEpoch => "SET-CONFIG-EPOCH",
      CommandKind::ClusterSetSlot => "SETSLOT",
      CommandKind::ClusterReplicas => "REPLICAS",
      CommandKind::ClusterSlots => "SLOTS",
      CommandKind::ClusterBumpEpoch => "BUMPEPOCH",
      CommandKind::ClusterFlushSlots => "FLUSHSLOTS",
      CommandKind::ClusterMyID => "MYID",
      CommandKind::ClientID => "ID",
      CommandKind::ClientInfo => "INFO",
      CommandKind::ClientKill => "KILL",
      CommandKind::ClientList => "LIST",
      CommandKind::ClientGetName => "GETNAME",
      CommandKind::ClientPause => "PAUSE",
      CommandKind::ClientUnpause => "UNPAUSE",
      CommandKind::ClientUnblock => "UNBLOCK",
      CommandKind::ClientReply => "REPLY",
      CommandKind::ClientSetname => "SETNAME",
      CommandKind::ConfigGet => "GET",
      CommandKind::ConfigRewrite => "REWRITE",
      CommandKind::ClientGetRedir => "GETREDIR",
      CommandKind::ClientTracking => "TRACKING",
      CommandKind::ClientTrackingInfo => "TRACKINGINFO",
      CommandKind::ClientCaching => "CACHING",
      CommandKind::ConfigSet => "SET",
      CommandKind::ConfigResetStat => "RESETSTAT",
      CommandKind::MemoryDoctor => "DOCTOR",
      CommandKind::MemoryHelp => "HELP",
      CommandKind::MemoryUsage => "USAGE",
      CommandKind::MemoryMallocStats => "MALLOC-STATS",
      CommandKind::MemoryStats => "STATS",
      CommandKind::MemoryPurge => "PURGE",
      CommandKind::XinfoConsumers => "CONSUMERS",
      CommandKind::XinfoGroups => "GROUPS",
      CommandKind::XinfoStream => "STREAM",
      CommandKind::Xgroupcreate => "CREATE",
      CommandKind::XgroupCreateConsumer => "CREATECONSUMER",
      CommandKind::XgroupDelConsumer => "DELCONSUMER",
      CommandKind::XgroupDestroy => "DESTROY",
      CommandKind::XgroupSetId => "SETID",
      CommandKind::FunctionDelete => "DELETE",
      CommandKind::FunctionDump => "DUMP",
      CommandKind::FunctionFlush => "FLUSH",
      CommandKind::FunctionKill => "KILL",
      CommandKind::FunctionList => "LIST",
      CommandKind::FunctionLoad => "LOAD",
      CommandKind::FunctionRestore => "RESTORE",
      CommandKind::FunctionStats => "STATS",
      CommandKind::PubsubChannels => "CHANNELS",
      CommandKind::PubsubNumpat => "NUMPAT",
      CommandKind::PubsubNumsub => "NUMSUB",
      CommandKind::PubsubShardchannels => "SHARDCHANNELS",
      CommandKind::PubsubShardnumsub => "SHARDNUMSUB",
      CommandKind::_FunctionLoadCluster => "LOAD",
      CommandKind::_FunctionFlushCluster => "FLUSH",
      CommandKind::_FunctionDeleteCluster => "DELETE",
      CommandKind::_FunctionRestoreCluster => "RESTORE",
      CommandKind::_ClientTrackingCluster => "TRACKING",
      CommandKind::JsonDebugMemory => "MEMORY",
      CommandKind::FtConfigGet => "GET",
      CommandKind::FtConfigSet => "SET",
      CommandKind::FtCursorDel => "DEL",
      CommandKind::FtCursorRead => "READ",
      _ => return None,
    };

    Some(utils::static_str(s))
  }

  pub fn use_random_cluster_node(&self) -> bool {
    matches!(
      *self,
      CommandKind::Publish | CommandKind::Ping | CommandKind::Info | CommandKind::FlushAll | CommandKind::FlushDB
    )
  }

  pub fn is_blocking(&self) -> bool {
    match *self {
      CommandKind::BlPop
      | CommandKind::BrPop
      | CommandKind::BrPopLPush
      | CommandKind::BlMove
      | CommandKind::BzPopMin
      | CommandKind::BzPopMax
      | CommandKind::BlmPop
      | CommandKind::BzmPop
      | CommandKind::Fcall
      | CommandKind::FcallRO
      | CommandKind::Wait => true,
      // default is false, but can be changed by the BLOCKING args. the RedisCommand::can_pipeline function checks the
      // args too.
      CommandKind::Xread | CommandKind::Xreadgroup => false,
      CommandKind::_Custom(ref kind) => kind.blocking,
      _ => false,
    }
  }

  pub fn force_all_cluster_nodes(&self) -> bool {
    matches!(
      *self,
      CommandKind::_FlushAllCluster
        | CommandKind::_AuthAllCluster
        | CommandKind::_ScriptFlushCluster
        | CommandKind::_ScriptKillCluster
        | CommandKind::_HelloAllCluster(_)
        | CommandKind::_ClientTrackingCluster
        | CommandKind::_ScriptLoadCluster
        | CommandKind::_FunctionFlushCluster
        | CommandKind::_FunctionDeleteCluster
        | CommandKind::_FunctionRestoreCluster
        | CommandKind::_FunctionLoadCluster
    )
  }

  pub fn should_flush(&self) -> bool {
    matches!(
      *self,
      CommandKind::Quit
        | CommandKind::Shutdown
        | CommandKind::Ping
        | CommandKind::Auth
        | CommandKind::_Hello(_)
        | CommandKind::Exec
        | CommandKind::Discard
        | CommandKind::Eval
        | CommandKind::EvalSha
        | CommandKind::Fcall
        | CommandKind::FcallRO
        | CommandKind::_Custom(_)
    )
  }

  pub fn is_pubsub(&self) -> bool {
    matches!(
      *self,
      CommandKind::Subscribe
        | CommandKind::Unsubscribe
        | CommandKind::Psubscribe
        | CommandKind::Punsubscribe
        | CommandKind::Ssubscribe
        | CommandKind::Sunsubscribe
    )
  }

  pub fn can_pipeline(&self) -> bool {
    if self.is_blocking() || self.closes_connection() {
      false
    } else {
      match self {
        // make it easier to handle multiple potentially out-of-band responses
        CommandKind::Subscribe
        | CommandKind::Unsubscribe
        | CommandKind::Psubscribe
        | CommandKind::Punsubscribe
        | CommandKind::Ssubscribe
        | CommandKind::Sunsubscribe
        // https://redis.io/commands/eval#evalsha-in-the-context-of-pipelining
        | CommandKind::Eval
        | CommandKind::EvalSha
        | CommandKind::Auth
        | CommandKind::Fcall
        | CommandKind::FcallRO
        // makes it easier to avoid decoding in-flight responses with the wrong codec logic
        | CommandKind::_Hello(_) => false,
        _ => true,
      }
    }
  }

  pub fn is_eval(&self) -> bool {
    matches!(
      *self,
      CommandKind::EvalSha | CommandKind::Eval | CommandKind::Fcall | CommandKind::FcallRO
    )
  }
}

pub struct Command {
  /// The command and optional subcommand name.
  pub kind:                   CommandKind,
  /// The policy to apply when handling the response.
  pub response:               ResponseKind,
  /// The policy to use when hashing the arguments for cluster routing.
  pub hasher:                 ClusterHash,
  /// The provided arguments.
  ///
  /// Some commands store arguments differently. Callers should use `self.args()` to account for this.
  pub arguments:              Vec<Value>,
  /// The number of times the command has been written to a socket.
  pub write_attempts:         u32,
  /// The number of write attempts remaining.
  pub attempts_remaining:     u32,
  /// The number of cluster redirections remaining.
  pub redirections_remaining: u32,
  /// Whether the command can be pipelined.
  ///
  /// Also used for commands like XREAD that block based on an argument.
  pub can_pipeline:           bool,
  /// Whether to fail fast without retries if the connection ever closes unexpectedly.
  pub fail_fast:              bool,
  /// The internal ID of a transaction.
  pub transaction_id:         Option<u64>,
  /// The timeout duration provided by the `with_options` interface.
  pub timeout_dur:            Option<Duration>,
  /// Whether the command has timed out from the perspective of the caller.
  pub timed_out:              RefCount<AtomicBool>,
  /// A timestamp of when the command was last written to the socket.
  pub network_start:          Option<Instant>,
  /// Whether to route the command to a replica, if possible.
  pub use_replica:            bool,
  /// Only send the command to the provided server.
  pub cluster_node:           Option<Server>,
  /// A timestamp of when the command was first created from the public interface.
  #[cfg(feature = "metrics")]
  pub created:                Instant,
  /// Tracing state that has to carry over across writer/reader tasks to track certain fields (response size, etc).
  #[cfg(feature = "partial-tracing")]
  pub traces:                 CommandTraces,
  /// A counter to differentiate unique commands.
  #[cfg(feature = "debug-ids")]
  pub counter:                usize,
  /// Whether to send a `CLIENT CACHING yes|no` before the command.
  #[cfg(feature = "i-tracking")]
  pub caching:                Option<bool>,
}

impl fmt::Debug for Command {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut formatter = f.debug_struct("RedisCommand");
    formatter
      .field("command", &self.kind.to_str_debug())
      .field("attempts_remaining", &self.attempts_remaining)
      .field("redirections_remaining", &self.redirections_remaining)
      .field("can_pipeline", &self.can_pipeline)
      .field("write_attempts", &self.write_attempts)
      .field("timeout_dur", &self.timeout_dur)
      .field("cluster_node", &self.cluster_node)
      .field("cluster_hash", &self.hasher)
      .field("use_replica", &self.use_replica)
      .field("fail_fast", &self.fail_fast);

    #[cfg(feature = "network-logs")]
    formatter.field("arguments", &self.args());

    formatter.finish()
  }
}

impl fmt::Display for Command {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.kind.to_str_debug())
  }
}

impl From<CommandKind> for Command {
  fn from(kind: CommandKind) -> Self {
    (kind, Vec::new()).into()
  }
}

impl From<(CommandKind, Vec<Value>)> for Command {
  fn from((kind, arguments): (CommandKind, Vec<Value>)) -> Self {
    Command {
      kind,
      arguments,
      timed_out: RefCount::new(AtomicBool::new(false)),
      timeout_dur: None,
      response: ResponseKind::Respond(None),
      hasher: ClusterHash::default(),
      attempts_remaining: 0,
      redirections_remaining: 0,
      can_pipeline: true,
      transaction_id: None,
      use_replica: false,
      cluster_node: None,
      network_start: None,
      write_attempts: 0,
      fail_fast: false,
      #[cfg(feature = "metrics")]
      created: Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces: CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter: command_counter(),
      #[cfg(feature = "i-tracking")]
      caching: None,
    }
  }
}

impl From<(CommandKind, Vec<Value>, ResponseSender)> for Command {
  fn from((kind, arguments, tx): (CommandKind, Vec<Value>, ResponseSender)) -> Self {
    Command {
      kind,
      arguments,
      response: ResponseKind::Respond(Some(tx)),
      timed_out: RefCount::new(AtomicBool::new(false)),
      timeout_dur: None,
      hasher: ClusterHash::default(),
      attempts_remaining: 0,
      redirections_remaining: 0,
      can_pipeline: true,
      transaction_id: None,
      use_replica: false,
      cluster_node: None,
      network_start: None,
      write_attempts: 0,
      fail_fast: false,
      #[cfg(feature = "metrics")]
      created: Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces: CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter: command_counter(),
      #[cfg(feature = "i-tracking")]
      caching: None,
    }
  }
}

impl From<(CommandKind, Vec<Value>, ResponseKind)> for Command {
  fn from((kind, arguments, response): (CommandKind, Vec<Value>, ResponseKind)) -> Self {
    Command {
      kind,
      arguments,
      response,
      timed_out: RefCount::new(AtomicBool::new(false)),
      timeout_dur: None,
      hasher: ClusterHash::default(),
      attempts_remaining: 0,
      redirections_remaining: 0,
      can_pipeline: true,
      transaction_id: None,
      use_replica: false,
      cluster_node: None,
      network_start: None,
      write_attempts: 0,
      fail_fast: false,
      #[cfg(feature = "metrics")]
      created: Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces: CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter: command_counter(),
      #[cfg(feature = "i-tracking")]
      caching: None,
    }
  }
}

impl Command {
  /// Create a new command without a response handling policy.
  pub fn new(kind: CommandKind, arguments: Vec<Value>) -> Self {
    Command {
      kind,
      arguments,
      timed_out: RefCount::new(AtomicBool::new(false)),
      timeout_dur: None,
      response: ResponseKind::Respond(None),
      hasher: ClusterHash::default(),
      attempts_remaining: 1,
      redirections_remaining: 1,
      can_pipeline: true,
      transaction_id: None,
      use_replica: false,
      cluster_node: None,
      network_start: None,
      write_attempts: 0,
      fail_fast: false,
      #[cfg(feature = "metrics")]
      created: Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces: CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter: command_counter(),
      #[cfg(feature = "i-tracking")]
      caching: None,
    }
  }

  /// Create a new empty `ASKING` command.
  pub fn new_asking(hash_slot: u16) -> Self {
    Command {
      kind:                                       CommandKind::Asking,
      hasher:                                     ClusterHash::Custom(hash_slot),
      arguments:                                  Vec::new(),
      timed_out:                                  RefCount::new(AtomicBool::new(false)),
      timeout_dur:                                None,
      response:                                   ResponseKind::Respond(None),
      attempts_remaining:                         1,
      redirections_remaining:                     1,
      can_pipeline:                               true,
      transaction_id:                             None,
      use_replica:                                false,
      cluster_node:                               None,
      network_start:                              None,
      write_attempts:                             0,
      fail_fast:                                  false,
      #[cfg(feature = "metrics")]
      created:                                    Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces:                                     CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter:                                    command_counter(),
      #[cfg(feature = "i-tracking")]
      caching:                                    None,
    }
  }

  /// Whether the command should be sent to all cluster nodes concurrently.
  pub fn is_all_cluster_nodes(&self) -> bool {
    self.kind.force_all_cluster_nodes()
      || match self.kind {
        // since we don't know the hash slot we send this to all nodes
        CommandKind::Sunsubscribe => self.arguments.is_empty(),
        _ => false,
      }
  }

  /// Whether errors writing the command should be returned to the caller.
  pub fn should_finish_with_error(&self, inner: &RefCount<ClientInner>) -> bool {
    self.fail_fast || self.attempts_remaining == 0 || inner.policy.read().is_none()
  }

  /// Increment and check the number of write attempts.
  pub fn decr_check_attempted(&mut self) -> Result<(), Error> {
    if self.attempts_remaining == 0 {
      Err(Error::new(ErrorKind::Unknown, "Too many failed write attempts."))
    } else {
      self.attempts_remaining -= 1;
      Ok(())
    }
  }

  pub fn in_transaction(&self) -> bool {
    self.transaction_id.is_some()
  }

  pub fn decr_check_redirections(&mut self) -> Result<(), Error> {
    if self.redirections_remaining == 0 {
      Err(Error::new(ErrorKind::Routing, "Too many redirections."))
    } else {
      self.redirections_remaining -= 1;
      Ok(())
    }
  }

  /// Read the arguments associated with the command.
  pub fn args(&self) -> &Vec<Value> {
    match self.response {
      ResponseKind::ValueScan(ref inner) => &inner.args,
      ResponseKind::KeyScan(ref inner) => &inner.args,
      ResponseKind::KeyScanBuffered(ref inner) => &inner.args,
      _ => &self.arguments,
    }
  }

  /// Whether the command blocks the connection.
  pub fn blocks_connection(&self) -> bool {
    self.transaction_id.is_none()
      && (self.kind.is_blocking()
        || match self.kind {
          CommandKind::Xread | CommandKind::Xreadgroup => !self.can_pipeline,
          _ => false,
        })
  }

  /// Whether the command may receive response frames.
  ///
  /// Currently, the pubsub subscription commands (other than `SSUBSCRIBE`) all fall into this category since their
  /// responses arrive out-of-band.
  // `SSUBSCRIBE` is not included here so that we can follow cluster redirections. this works as long as we never
  // pipeline `SSUBSCRIBE`.
  pub fn has_no_responses(&self) -> bool {
    matches!(
      self.kind,
      CommandKind::Subscribe
        | CommandKind::Unsubscribe
        | CommandKind::Psubscribe
        | CommandKind::Punsubscribe
        | CommandKind::Sunsubscribe
    )
  }

  /// Take the arguments from this command.
  pub fn take_args(&mut self) -> Vec<Value> {
    match self.response {
      ResponseKind::ValueScan(ref mut inner) => inner.args.drain(..).collect(),
      ResponseKind::KeyScan(ref mut inner) => inner.args.drain(..).collect(),
      ResponseKind::KeyScanBuffered(ref mut inner) => inner.args.drain(..).collect(),
      _ => self.arguments.drain(..).collect(),
    }
  }

  /// Take the response handler, replacing it with `ResponseKind::Skip`.
  pub fn take_response(&mut self) -> ResponseKind {
    mem::replace(&mut self.response, ResponseKind::Skip)
  }

  /// Clone the command, supporting commands with shared response state.
  ///
  /// Note: this will **not** clone the router channel.
  pub fn duplicate(&self, response: ResponseKind) -> Self {
    Command {
      timed_out: RefCount::new(AtomicBool::new(false)),
      kind: self.kind.clone(),
      arguments: self.arguments.clone(),
      hasher: self.hasher.clone(),
      transaction_id: self.transaction_id,
      attempts_remaining: self.attempts_remaining,
      redirections_remaining: self.redirections_remaining,
      timeout_dur: self.timeout_dur,
      can_pipeline: self.can_pipeline,
      cluster_node: self.cluster_node.clone(),
      fail_fast: self.fail_fast,
      response,
      use_replica: self.use_replica,
      write_attempts: self.write_attempts,
      network_start: self.network_start,
      #[cfg(feature = "metrics")]
      created: Instant::now(),
      #[cfg(feature = "partial-tracing")]
      traces: CommandTraces::default(),
      #[cfg(feature = "debug-ids")]
      counter: command_counter(),
      #[cfg(feature = "i-tracking")]
      caching: self.caching,
    }
  }

  /// Inherit connection and perf settings from the client.
  pub fn inherit_options(&mut self, inner: &RefCount<ClientInner>) {
    if self.attempts_remaining == 0 {
      self.attempts_remaining = inner.connection.max_command_attempts;
    }
    if self.redirections_remaining == 0 {
      self.redirections_remaining = inner.connection.max_redirections;
    }
    if self.timeout_dur.is_none() {
      let default_dur = inner.default_command_timeout();
      if !default_dur.is_zero() {
        self.timeout_dur = Some(default_dur);
      }
    }
  }

  /// Take the command tracing state for the `queued` span.
  #[cfg(feature = "full-tracing")]
  pub fn take_queued_span(&mut self) -> Option<trace::Span> {
    self.traces.queued.take()
  }

  /// Take the command tracing state for the `queued` span.
  #[cfg(not(feature = "full-tracing"))]
  pub fn take_queued_span(&mut self) -> Option<trace::disabled::Span> {
    None
  }

  /// Take the response sender from the command.
  ///
  /// Usually used for responding early without sending the command.
  pub fn take_responder(&mut self) -> Option<ResponseSender> {
    match self.response {
      ResponseKind::Respond(ref mut tx) => tx.take(),
      ResponseKind::Buffer { ref mut tx, .. } => tx.lock().take(),
      _ => None,
    }
  }

  #[cfg(feature = "mocks")]
  pub fn take_key_scan_tx(&mut self) -> Option<Sender<Result<ScanResult, Error>>> {
    match mem::replace(&mut self.response, ResponseKind::Skip) {
      ResponseKind::KeyScan(inner) => Some(inner.tx),
      _ => None,
    }
  }

  #[cfg(feature = "mocks")]
  pub fn take_key_scan_buffered_tx(&mut self) -> Option<Sender<Result<Key, Error>>> {
    match mem::replace(&mut self.response, ResponseKind::Skip) {
      ResponseKind::KeyScanBuffered(inner) => Some(inner.tx),
      _ => None,
    }
  }

  #[cfg(feature = "mocks")]
  pub fn take_value_scan_tx(&mut self) -> Option<Sender<Result<ValueScanResult, Error>>> {
    match mem::replace(&mut self.response, ResponseKind::Skip) {
      ResponseKind::ValueScan(inner) => Some(inner.tx),
      _ => None,
    }
  }

  /// Whether the command has a channel for sending responses to the caller.
  pub fn has_response_tx(&self) -> bool {
    match self.response {
      ResponseKind::Respond(ref r) => r.is_some(),
      ResponseKind::Buffer { ref tx, .. } => tx.lock().is_some(),
      _ => false,
    }
  }

  /// Respond to the caller, taking the response channel in the process.
  pub fn respond_to_caller(&mut self, result: Result<Resp3Frame, Error>) {
    match self.response {
      ResponseKind::KeyScanBuffered(ref inner) => {
        if let Err(error) = result {
          let _ = inner.tx.try_send(Err(error));
        }
      },
      ResponseKind::KeyScan(ref inner) => {
        if let Err(error) = result {
          let _ = inner.tx.try_send(Err(error));
        }
      },
      ResponseKind::ValueScan(ref inner) => {
        if let Err(error) = result {
          let _ = inner.tx.try_send(Err(error));
        }
      },
      _ =>
      {
        #[allow(unused_mut)]
        if let Some(mut tx) = self.take_responder() {
          let _ = tx.send(result);
        }
      },
    }
  }

  /// Read the first key in the arguments according to the `FirstKey` cluster hash policy.
  pub fn first_key(&self) -> Option<&[u8]> {
    ClusterHash::FirstKey.find_key(self.args())
  }

  /// Hash the arguments according to the command's cluster hash policy.
  pub fn cluster_hash(&self) -> Option<u16> {
    self
      .kind
      .custom_hash_slot()
      .or(self.scan_hash_slot())
      .or(self.hasher.hash(self.args()))
  }

  /// Read the custom hash slot assigned to a scan operation.
  pub fn scan_hash_slot(&self) -> Option<u16> {
    match self.response {
      ResponseKind::KeyScan(ref inner) => inner.hash_slot,
      ResponseKind::KeyScanBuffered(ref inner) => inner.hash_slot,
      _ => None,
    }
  }

  /// Convert to a single frame with an array of bulk strings (or null).
  pub fn to_frame(&self, is_resp3: bool) -> Result<ProtocolFrame, Error> {
    protocol_utils::command_to_frame(self, is_resp3)
  }

  /// Convert to a single frame with an array of bulk strings (or null), using a blocking task.
  #[cfg(all(feature = "blocking-encoding", not(feature = "glommio")))]
  pub fn to_frame_blocking(&self, is_resp3: bool, blocking_threshold: usize) -> Result<ProtocolFrame, Error> {
    let cmd_size = protocol_utils::args_size(self.args());

    if cmd_size >= blocking_threshold {
      trace!("Using blocking task to convert command to frame with size {}", cmd_size);
      tokio::task::block_in_place(|| protocol_utils::command_to_frame(self, is_resp3))
    } else {
      protocol_utils::command_to_frame(self, is_resp3)
    }
  }

  #[cfg(feature = "mocks")]
  pub fn to_mocked(&self) -> MockCommand {
    MockCommand {
      cmd:        self.kind.cmd_str(),
      subcommand: self.kind.subcommand_str(),
      args:       self.args().clone(),
    }
  }

  #[cfg(not(feature = "debug-ids"))]
  pub fn debug_id(&self) -> usize {
    0
  }

  #[cfg(feature = "debug-ids")]
  pub fn debug_id(&self) -> usize {
    self.counter
  }
}

/// A message sent from the front-end client to the router.
pub enum RouterCommand {
  /// Send a command to the server.
  Command(Command),
  /// Send a pipelined series of commands to the server.
  Pipeline { commands: Vec<Command> },
  /// Send a transaction to the server.
  // The inner command buffer will not contain the trailing `EXEC` command.
  #[cfg(feature = "transactions")]
  Transaction {
    id:             u64,
    commands:       Vec<Command>,
    abort_on_error: bool,
    tx:             ResponseSender,
  },
  /// Initiate a reconnection to the provided server, or all servers.
  Reconnect {
    server:  Option<Server>,
    force:   bool,
    tx:      Option<ResponseSender>,
    #[cfg(feature = "replicas")]
    replica: bool,
  },
  /// Retry a command after a `MOVED` error.
  Moved {
    slot:    u16,
    server:  Server,
    command: Command,
  },
  /// Retry a command after an `ASK` error.
  Ask {
    slot:    u16,
    server:  Server,
    command: Command,
  },
  /// Sync the cached cluster state with the server via `CLUSTER SLOTS`.
  SyncCluster { tx: OneshotSender<Result<(), Error>> },
  /// Force sync the replica routing table with the server(s).
  #[cfg(feature = "replicas")]
  SyncReplicas {
    tx:    OneshotSender<Result<(), Error>>,
    reset: bool,
  },
}

impl RouterCommand {
  /// Whether the command should check the health of the backing connections before being used.
  pub fn should_check_fail_fast(&self) -> bool {
    match self {
      RouterCommand::Command(command) => command.fail_fast,
      RouterCommand::Pipeline { commands, .. } => commands.first().map(|c| c.fail_fast).unwrap_or(false),
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { commands, .. } => commands.first().map(|c| c.fail_fast).unwrap_or(false),
      _ => false,
    }
  }

  /// Finish the command early with the provided error.
  #[allow(unused_mut)]
  pub fn finish_with_error(self, error: Error) {
    match self {
      RouterCommand::Command(mut command) => {
        command.respond_to_caller(Err(error));
      },
      RouterCommand::Pipeline { commands } => {
        for mut command in commands.into_iter() {
          command.respond_to_caller(Err(error.clone()));
        }
      },
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { mut tx, .. } => {
        if let Err(_) = tx.send(Err(error)) {
          warn!("Error responding early to transaction.");
        }
      },
      RouterCommand::Reconnect { tx: Some(mut tx), .. } => {
        if let Err(_) = tx.send(Err(error)) {
          warn!("Error responding early to reconnect command.");
        }
      },
      _ => {},
    }
  }

  /// Inherit settings from the configuration structs on `inner`.
  pub fn inherit_options(&mut self, inner: &RefCount<ClientInner>) {
    match self {
      RouterCommand::Command(ref mut cmd) => {
        cmd.inherit_options(inner);
      },
      RouterCommand::Pipeline { ref mut commands, .. } => {
        for cmd in commands.iter_mut() {
          cmd.inherit_options(inner);
        }
      },
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { ref mut commands, .. } => {
        for cmd in commands.iter_mut() {
          cmd.inherit_options(inner);
        }
      },
      _ => {},
    };
  }

  /// Apply a timeout to the response channel receiver based on the command and `inner` context.
  pub fn timeout_dur(&self) -> Option<Duration> {
    match self {
      RouterCommand::Command(ref command) => command.timeout_dur,
      RouterCommand::Pipeline { ref commands, .. } => commands.first().and_then(|c| c.timeout_dur),
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { ref commands, .. } => commands.first().and_then(|c| c.timeout_dur),
      _ => None,
    }
  }

  /// Cancel the underlying command and respond to the caller, if possible.
  pub fn cancel(self) {
    match self {
      RouterCommand::Command(mut command) => {
        let result = if command.kind == CommandKind::Quit {
          Ok(Resp3Frame::Null)
        } else {
          Err(Error::new_canceled())
        };

        command.respond_to_caller(result);
      },
      RouterCommand::Pipeline { mut commands } => {
        if let Some(mut command) = commands.pop() {
          command.respond_to_caller(Err(Error::new_canceled()));
        }
      },
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { tx, .. } => {
        let _ = tx.send(Err(Error::new_canceled()));
      },
      _ => {},
    }
  }
}

impl fmt::Debug for RouterCommand {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    let mut formatter = f.debug_struct("RouterCommand");

    match self {
      #[cfg(not(feature = "replicas"))]
      RouterCommand::Reconnect { server, force, .. } => {
        formatter
          .field("kind", &"Reconnect")
          .field("server", &server)
          .field("force", &force);
      },
      #[cfg(feature = "replicas")]
      RouterCommand::Reconnect {
        server, force, replica, ..
      } => {
        formatter
          .field("kind", &"Reconnect")
          .field("server", &server)
          .field("replica", &replica)
          .field("force", &force);
      },
      RouterCommand::SyncCluster { .. } => {
        formatter.field("kind", &"Sync Cluster");
      },
      #[cfg(feature = "transactions")]
      RouterCommand::Transaction { .. } => {
        formatter.field("kind", &"Transaction");
      },
      RouterCommand::Pipeline { .. } => {
        formatter.field("kind", &"Pipeline");
      },
      RouterCommand::Ask { .. } => {
        formatter.field("kind", &"Ask");
      },
      RouterCommand::Moved { .. } => {
        formatter.field("kind", &"Moved");
      },
      RouterCommand::Command(command) => {
        formatter
          .field("kind", &"Command")
          .field("command", &command.kind.to_str_debug());
      },
      #[cfg(feature = "replicas")]
      RouterCommand::SyncReplicas { reset, .. } => {
        formatter.field("kind", &"Sync Replicas");
        formatter.field("reset", &reset);
      },
    };

    formatter.finish()
  }
}

impl From<Command> for RouterCommand {
  fn from(cmd: Command) -> Self {
    RouterCommand::Command(cmd)
  }
}
