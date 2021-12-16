------------------------ MODULE query_deduplication ------------------------


EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANT Proc
CONSTANT Keys
(*

\* from https://probablydance.com/2020/10/31/using-tla-in-the-real-world-to-understand-a-glibc-bug/
--algorithm Spinlock 
{
  variables
    cache_lock = 0;
    wait_map_lock = 1;
    locks = [i \in {cache_lock, wait_map_lock} |-> FALSE];
    \* set to false for every possible key at first
    cache = [ i \in Keys |-> FALSE ];
    wait_map = [ i \in Keys |-> {} ];
    channels = [i \in Proc |-> FALSE ];
    tested_keys = {};
 
   define {

    MutualExclusion == \A i, j \in Proc : 
                         (i # j) => ~ /\ pc[i] = "cs"
                                      /\ pc[j] = "cs"
                                      
    DeadlockFreedom == 
        \A i \in Proc : 
          (pc[i] = "exchange") ~> (\E j \in Proc : pc[j] = "cs")
    
    
    \*AllFinished == <>(\A proc \in Proc: pc[proc] = "Done")
    \*AllKeysLoaded == <>(\A k \in tested_keys: cache[k])
  };

  procedure lock(idx = 0)
  {
    exchange:
      await ~locks[idx];
    check:
      if(~locks[idx]) {
        locks[idx] := TRUE;
        return;
      } else {
        goto exchange;
      };
  };

  procedure unlock(idx = 0)
  {
    reset_state:
      locks[idx] := FALSE;
      return;
  }
  
  fair process (P \in Proc)
  variables Key \in Keys, pr = 0;
  {
  
 
    start:
        either {
            \* expire one key
            call lock(cache_lock);
            expiration_lock_1:
                cache[Key] := FALSE;
                call unlock(cache_lock);
            expiration_done:
                goto finished;
        }
        or {
        \* we test at the end if all keys have been loaded
        tested_keys := tested_keys \union {Key};
        
        \* let mut locked_cache = self.cached.lock().await;
        call lock(cache_lock);
        };
        
    lock_1:    
        \*if let Some(value) = locked_cache.get(&key).cloned() { return value }
        if (cache[Key]) {
            call unlock(cache_lock);
            cache_hit:
                goto finished;
        };
        
    
    lock_2:
        \* let mut locked_wait_map = self.wait_map.lock().await;
        call lock(wait_map_lock);
    
    lock_3:
        \* drop(locked_cache);
        call unlock(cache_lock);
    
    lookup_wait_map:
        \* match locked_wait_map.get_mut(&key) {
        \*    Some(waiter) => {
        if (Cardinality(wait_map[Key]) > 0)
        {
            wait_map_subscribe:
                \* we register to reeive a notification when the value is available
                \* let mut receiver = waiter.subscribe();
                wait_map[Key] := wait_map[Key] \union {self};
                \* drop(locked_wait_map);
                call unlock(wait_map_lock);
            
            waiting_for_value:
                \* let (recv_key, recv_value) = receiver.recv().await
                await channels[self];
                goto finished;
    
        }
        else
        {
            wait_map_register:
                \* the wait map should be emty for this key
                assert wait_map[Key] = {};
                \* we should not have a value yet
                assert ~cache[Key];
                
                \* let (tx, _rx) = broadcast::channel(1);
                \* locked_wait_map.insert(key.clone(), tx.clone());
                wait_map[Key] := {self};
                \* drop(locked_wait_map);
                call unlock(wait_map_lock);
                
            fetching_value:
                \* let value = self.resolver.retrieve(key.clone()).await;
                skip;
            
            inserting_value_lock_cache:
                \* let mut locked_cache = self.cached.lock().await;
                call lock(cache_lock);
                
            inserting_value:
                \* locked_cache.put(key.clone(), value.clone());
                cache[Key] := TRUE;
            
                \* let mut locked_wait_map = self.wait_map.lock().await;
                call lock(wait_map_lock);
 
            broadcast:
                \* locked_wait_map.remove(&key);
                wait_map[Key] := wait_map[Key] \ {self};
                
                \* notify all waiting processes
                \*  tokio::task::spawn_blocking(move || {
                \*    tx.send((key, broadcast_value))
                \*        .expect("there is always at least one receiver alive, the _rx guard; qed")
                \*})
            notify_all:
                while(Cardinality(wait_map[Key]) > 0) {

                    pr:= CHOOSE p \in wait_map[Key]: TRUE;
                    channels[pr] := TRUE;
                    wait_map[Key] := wait_map[Key] \ {pr};
                };
            
                \* locks are automatically dropped
                call unlock(wait_map_lock);
            unlock_1:
                call unlock(cache_lock);
            unlock_2:    
                goto finished;
                
            
        };
    
    finished:
        skip;
   }
}
*)
\* BEGIN TRANSLATION (chksum(pcal) = "9854a4c7" /\ chksum(tla) = "fb95c68d")
\* Parameter index of procedure lock at line 38 col 18 changed to index_
VARIABLES cache_lock, wait_map_lock, locks, cache, wait_map, channels, 
          tested_keys, pc, stack

(* define statement *)
MutualExclusion == \A i, j \in Proc :
                     (i # j) => ~ /\ pc[i] = "cs"
                                  /\ pc[j] = "cs"

DeadlockFreedom ==
    \A i \in Proc :
      (pc[i] = "exchange") ~> (\E j \in Proc : pc[j] = "cs")

VARIABLES index_, old_value, index, Key

vars == << cache_lock, wait_map_lock, locks, cache, wait_map, channels, 
           tested_keys, pc, stack, index_, old_value, index, Key >>

ProcSet == (Proc)

Init == (* Global variables *)
        /\ cache_lock = 0
        /\ wait_map_lock = 1
        /\ locks = [i \in {cache_lock, wait_map_lock} |-> FALSE]
        /\ cache = [ i \in Keys |-> FALSE ]
        /\ wait_map = [ i \in Keys |-> {} ]
        /\ channels = [i \in Proc |-> FALSE ]
        /\ tested_keys = {}
        (* Procedure lock *)
        /\ index_ = [ self \in ProcSet |-> 0]
        /\ old_value = [ self \in ProcSet |-> FALSE]
        (* Procedure unlock *)
        /\ index = [ self \in ProcSet |-> 0]
        (* Process P *)
        /\ Key \in [Proc -> Keys]
        /\ stack = [self \in ProcSet |-> << >>]
        /\ pc = [self \in ProcSet |-> "start"]

exchange(self) == /\ pc[self] = "exchange"
                  /\ old_value' = [old_value EXCEPT ![self] = locks[index_[self]]]
                  /\ locks' = [locks EXCEPT ![index_[self]] = TRUE]
                  /\ pc' = [pc EXCEPT ![self] = "check"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, cache, wait_map, 
                                  channels, tested_keys, stack, index_, index, 
                                  Key >>

check(self) == /\ pc[self] = "check"
               /\ IF old_value[self]
                     THEN /\ pc' = [pc EXCEPT ![self] = "exchange"]
                          /\ UNCHANGED << stack, index_, old_value >>
                     ELSE /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                          /\ old_value' = [old_value EXCEPT ![self] = Head(stack[self]).old_value]
                          /\ index_' = [index_ EXCEPT ![self] = Head(stack[self]).index_]
                          /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
               /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                               wait_map, channels, tested_keys, index, Key >>

lock(self) == exchange(self) \/ check(self)

reset_state(self) == /\ pc[self] = "reset_state"
                     /\ locks' = [locks EXCEPT ![index[self]] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                     /\ index' = [index EXCEPT ![self] = Head(stack[self]).index]
                     /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                     /\ UNCHANGED << cache_lock, wait_map_lock, cache, 
                                     wait_map, channels, tested_keys, index_, 
                                     old_value, Key >>

unlock(self) == reset_state(self)

start(self) == /\ pc[self] = "start"
               /\ \/ /\ /\ index_' = [index_ EXCEPT ![self] = cache_lock]
                        /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                 pc        |->  "expiration_lock_1",
                                                                 old_value |->  old_value[self],
                                                                 index_    |->  index_[self] ] >>
                                                             \o stack[self]]
                     /\ old_value' = [old_value EXCEPT ![self] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = "exchange"]
                     /\ UNCHANGED tested_keys
                  \/ /\ tested_keys' = (tested_keys \union {Key[self]})
                     /\ /\ index_' = [index_ EXCEPT ![self] = cache_lock]
                        /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                 pc        |->  "lock_1",
                                                                 old_value |->  old_value[self],
                                                                 index_    |->  index_[self] ] >>
                                                             \o stack[self]]
                     /\ old_value' = [old_value EXCEPT ![self] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = "exchange"]
               /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                               wait_map, channels, index, Key >>

expiration_lock_1(self) == /\ pc[self] = "expiration_lock_1"
                           /\ cache' = [cache EXCEPT ![Key[self]] = FALSE]
                           /\ /\ index' = [index EXCEPT ![self] = cache_lock]
                              /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                       pc        |->  "expiration_done",
                                                                       index     |->  index[self] ] >>
                                                                   \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           wait_map, channels, tested_keys, 
                                           index_, old_value, Key >>

expiration_done(self) == /\ pc[self] = "expiration_done"
                         /\ pc' = [pc EXCEPT ![self] = "finished"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         cache, wait_map, channels, 
                                         tested_keys, stack, index_, old_value, 
                                         index, Key >>

lock_1(self) == /\ pc[self] = "lock_1"
                /\ IF cache[Key[self]]
                      THEN /\ /\ index' = [index EXCEPT ![self] = cache_lock]
                              /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                       pc        |->  "cache_hit",
                                                                       index     |->  index[self] ] >>
                                                                   \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                      ELSE /\ pc' = [pc EXCEPT ![self] = "lock_2"]
                           /\ UNCHANGED << stack, index >>
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                wait_map, channels, tested_keys, index_, 
                                old_value, Key >>

cache_hit(self) == /\ pc[self] = "cache_hit"
                   /\ pc' = [pc EXCEPT ![self] = "finished"]
                   /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                   wait_map, channels, tested_keys, stack, 
                                   index_, old_value, index, Key >>

lock_2(self) == /\ pc[self] = "lock_2"
                /\ /\ index_' = [index_ EXCEPT ![self] = wait_map_lock]
                   /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                            pc        |->  "lock_3",
                                                            old_value |->  old_value[self],
                                                            index_    |->  index_[self] ] >>
                                                        \o stack[self]]
                /\ old_value' = [old_value EXCEPT ![self] = FALSE]
                /\ pc' = [pc EXCEPT ![self] = "exchange"]
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                wait_map, channels, tested_keys, index, Key >>

lock_3(self) == /\ pc[self] = "lock_3"
                /\ /\ index' = [index EXCEPT ![self] = cache_lock]
                   /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                            pc        |->  "lookup_wait_map",
                                                            index     |->  index[self] ] >>
                                                        \o stack[self]]
                /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                wait_map, channels, tested_keys, index_, 
                                old_value, Key >>

lookup_wait_map(self) == /\ pc[self] = "lookup_wait_map"
                         /\ IF Cardinality(wait_map[Key[self]]) > 0
                               THEN /\ pc' = [pc EXCEPT ![self] = "wait_map_subscribe"]
                               ELSE /\ pc' = [pc EXCEPT ![self] = "wait_map_register"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         cache, wait_map, channels, 
                                         tested_keys, stack, index_, old_value, 
                                         index, Key >>

wait_map_subscribe(self) == /\ pc[self] = "wait_map_subscribe"
                            /\ wait_map' = [wait_map EXCEPT ![Key[self]] = wait_map[Key[self]] \union {self}]
                            /\ /\ index' = [index EXCEPT ![self] = wait_map_lock]
                               /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                        pc        |->  "waiting_for_value",
                                                                        index     |->  index[self] ] >>
                                                                    \o stack[self]]
                            /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                            /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                            cache, channels, tested_keys, 
                                            index_, old_value, Key >>

waiting_for_value(self) == /\ pc[self] = "waiting_for_value"
                           /\ channels[self]
                           /\ pc' = [pc EXCEPT ![self] = "finished"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           cache, wait_map, channels, 
                                           tested_keys, stack, index_, 
                                           old_value, index, Key >>

wait_map_register(self) == /\ pc[self] = "wait_map_register"
                           /\ Assert(wait_map[Key[self]] = {}, 
                                     "Failure of assertion at line 124, column 17.")
                           /\ Assert(~cache[Key[self]], 
                                     "Failure of assertion at line 126, column 17.")
                           /\ wait_map' = [wait_map EXCEPT ![Key[self]] = {self}]
                           /\ /\ index' = [index EXCEPT ![self] = wait_map_lock]
                              /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                       pc        |->  "fetching_value",
                                                                       index     |->  index[self] ] >>
                                                                   \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           cache, channels, tested_keys, 
                                           index_, old_value, Key >>

fetching_value(self) == /\ pc[self] = "fetching_value"
                        /\ TRUE
                        /\ pc' = [pc EXCEPT ![self] = "inserting_value_lock_cache"]
                        /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                        cache, wait_map, channels, tested_keys, 
                                        stack, index_, old_value, index, Key >>

inserting_value_lock_cache(self) == /\ pc[self] = "inserting_value_lock_cache"
                                    /\ /\ index_' = [index_ EXCEPT ![self] = cache_lock]
                                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                                pc        |->  "inserting_value",
                                                                                old_value |->  old_value[self],
                                                                                index_    |->  index_[self] ] >>
                                                                            \o stack[self]]
                                    /\ old_value' = [old_value EXCEPT ![self] = FALSE]
                                    /\ pc' = [pc EXCEPT ![self] = "exchange"]
                                    /\ UNCHANGED << cache_lock, wait_map_lock, 
                                                    locks, cache, wait_map, 
                                                    channels, tested_keys, 
                                                    index, Key >>

inserting_value(self) == /\ pc[self] = "inserting_value"
                         /\ cache' = [cache EXCEPT ![Key[self]] = TRUE]
                         /\ /\ index_' = [index_ EXCEPT ![self] = wait_map_lock]
                            /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                     pc        |->  "broadcast",
                                                                     old_value |->  old_value[self],
                                                                     index_    |->  index_[self] ] >>
                                                                 \o stack[self]]
                         /\ old_value' = [old_value EXCEPT ![self] = FALSE]
                         /\ pc' = [pc EXCEPT ![self] = "exchange"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         wait_map, channels, tested_keys, 
                                         index, Key >>

broadcast(self) == /\ pc[self] = "broadcast"
                   /\ wait_map' = [wait_map EXCEPT ![Key[self]] = wait_map[Key[self]] \ {self}]
                   /\ pc' = [pc EXCEPT ![self] = "notify_all"]
                   /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                   channels, tested_keys, stack, index_, 
                                   old_value, index, Key >>

notify_all(self) == /\ pc[self] = "notify_all"
                    /\ IF Cardinality(wait_map[Key[self]]) > 0
                          THEN /\ pc' = [pc EXCEPT ![self] = "notify"]
                               /\ UNCHANGED << stack, index >>
                          ELSE /\ /\ index' = [index EXCEPT ![self] = wait_map_lock]
                                  /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                           pc        |->  "unlock_1",
                                                                           index     |->  index[self] ] >>
                                                                       \o stack[self]]
                               /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                    /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                    wait_map, channels, tested_keys, index_, 
                                    old_value, Key >>

notify(self) == /\ pc[self] = "notify"
                /\ \E proc \in wait_map[Key[self]]:
                     /\ channels' = [channels EXCEPT ![proc] = TRUE]
                     /\ wait_map' = [wait_map EXCEPT ![Key[self]] = wait_map[Key[self]] \ {proc}]
                /\ pc' = [pc EXCEPT ![self] = "notify_all"]
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                tested_keys, stack, index_, old_value, index, 
                                Key >>

unlock_1(self) == /\ pc[self] = "unlock_1"
                  /\ /\ index' = [index EXCEPT ![self] = cache_lock]
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                              pc        |->  "unlock_2",
                                                              index     |->  index[self] ] >>
                                                          \o stack[self]]
                  /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, index_, 
                                  old_value, Key >>

unlock_2(self) == /\ pc[self] = "unlock_2"
                  /\ pc' = [pc EXCEPT ![self] = "finished"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, stack, 
                                  index_, old_value, index, Key >>

finished(self) == /\ pc[self] = "finished"
                  /\ TRUE
                  /\ pc' = [pc EXCEPT ![self] = "Done"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, stack, 
                                  index_, old_value, index, Key >>

P(self) == start(self) \/ expiration_lock_1(self) \/ expiration_done(self)
              \/ lock_1(self) \/ cache_hit(self) \/ lock_2(self)
              \/ lock_3(self) \/ lookup_wait_map(self)
              \/ wait_map_subscribe(self) \/ waiting_for_value(self)
              \/ wait_map_register(self) \/ fetching_value(self)
              \/ inserting_value_lock_cache(self) \/ inserting_value(self)
              \/ broadcast(self) \/ notify_all(self) \/ notify(self)
              \/ unlock_1(self) \/ unlock_2(self) \/ finished(self)

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == (\E self \in ProcSet: lock(self) \/ unlock(self))
           \/ (\E self \in Proc: P(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ \A self \in Proc : WF_vars(P(self)) /\ WF_vars(lock(self)) /\ WF_vars(unlock(self))

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 



=============================================================================
\* Modification History
\* Last modified Tue Dec 14 14:43:54 CET 2021 by geal
\* Created Sat Dec 11 13:04:21 CET 2021 by geal

      
