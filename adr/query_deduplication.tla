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
  variables Key \in Keys, local_wait_map = {};
  {
  
    start:
        \* let mut locked_cache = self.cached.lock().await;
        call lock(cache_lock);

    cache_locked:
        \*if let Some(value) = locked_cache.get(&key).cloned() { return value }
        if (cache[Key]) {
            \* if there is a value for the key, we test two possible behaviours
            \* expire the key, or assume a normal task that will just return after
            \* receiving the value
            either {
                \* expire one key
                expiration_lock_1:
                    cache[Key] := FALSE;
                    call unlock(cache_lock);

                expiration_done:
                    goto finished;
            }
            or {
                \*if let Some(value) = locked_cache.get(&key).cloned() { return value }
                call unlock(cache_lock);
                cache_hit:
                    goto finished;
            }
        };


    cache_miss:
        \* we test at the end if all keys have been loaded
        tested_keys := tested_keys \union {Key};

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
 
            remove_wait_map:
                \* locked_wait_map.remove(&key);
                \* locks are dropped right after last use

                local_wait_map := wait_map[Key] \ { self };
                wait_map[Key] := {};

                call unlock(wait_map_lock);

            unlock_1:
                call unlock(cache_lock);

                \* notify all waiting processes
                \*  tokio::task::spawn_blocking(move || {
                \*    tx.send((key, broadcast_value))
                \*        .expect("there is always at least one receiver alive, the _rx guard; qed")
                \*})
            notify_all:
                while(Cardinality(local_wait_map) > 0) {
                    notify:
                        with (proc \in local_wait_map) {
                            channels[proc] := TRUE;
                            local_wait_map := local_wait_map \ {proc};
                        }
                };
            
                goto finished;
        };

    finished:
        skip;
   }
}
*)
\* BEGIN TRANSLATION (chksum(pcal) = "4471d3ed" /\ chksum(tla) = "6aa3bd7d")
\* Parameter idx of procedure lock at line 63 col 18 changed to idx_
VARIABLES cache_lock, wait_map_lock, locks, cache, wait_map, channels, 
          tested_keys, pc, stack

(* define statement *)
MutualExclusion == \A i, j \in Proc :
                     (i # j) => ~ /\ pc[i] = "cs"
                                  /\ pc[j] = "cs"

DeadlockFreedom ==
    \A i \in Proc :
      (pc[i] = "exchange") ~> (\E j \in Proc : pc[j] = "cs")



AllKeysLoaded == <>(\A k \in tested_keys: cache[k])

VARIABLES idx_, idx, Key, local_wait_map

vars == << cache_lock, wait_map_lock, locks, cache, wait_map, channels, 
           tested_keys, pc, stack, idx_, idx, Key, local_wait_map >>

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
        /\ idx_ = [ self \in ProcSet |-> 0]
        (* Procedure unlock *)
        /\ idx = [ self \in ProcSet |-> 0]
        (* Process P *)
        /\ Key \in [Proc -> Keys]
        /\ local_wait_map = [self \in Proc |-> {}]
        /\ stack = [self \in ProcSet |-> << >>]
        /\ pc = [self \in ProcSet |-> "start"]

exchange(self) == /\ pc[self] = "exchange"
                  /\ ~locks[idx_[self]]
                  /\ pc' = [pc EXCEPT ![self] = "check"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, stack, idx_, 
                                  idx, Key, local_wait_map >>

check(self) == /\ pc[self] = "check"
               /\ IF ~locks[idx_[self]]
                     THEN /\ locks' = [locks EXCEPT ![idx_[self]] = TRUE]
                          /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                          /\ idx_' = [idx_ EXCEPT ![self] = Head(stack[self]).idx_]
                          /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                     ELSE /\ pc' = [pc EXCEPT ![self] = "exchange"]
                          /\ UNCHANGED << locks, stack, idx_ >>
               /\ UNCHANGED << cache_lock, wait_map_lock, cache, wait_map, 
                               channels, tested_keys, idx, Key, local_wait_map >>

lock(self) == exchange(self) \/ check(self)

reset_state(self) == /\ pc[self] = "reset_state"
                     /\ locks' = [locks EXCEPT ![idx[self]] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                     /\ idx' = [idx EXCEPT ![self] = Head(stack[self]).idx]
                     /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                     /\ UNCHANGED << cache_lock, wait_map_lock, cache, 
                                     wait_map, channels, tested_keys, idx_, 
                                     Key, local_wait_map >>

unlock(self) == reset_state(self)

start(self) == /\ pc[self] = "start"
               /\ /\ idx_' = [idx_ EXCEPT ![self] = cache_lock]
                  /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                           pc        |->  "cache_locked",
                                                           idx_      |->  idx_[self] ] >>
                                                       \o stack[self]]
               /\ pc' = [pc EXCEPT ![self] = "exchange"]
               /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                               wait_map, channels, tested_keys, idx, Key, 
                               local_wait_map >>

cache_locked(self) == /\ pc[self] = "cache_locked"
                      /\ IF cache[Key[self]]
                            THEN /\ \/ /\ pc' = [pc EXCEPT ![self] = "expiration_lock_1"]
                                       /\ UNCHANGED <<stack, idx>>
                                    \/ /\ /\ idx' = [idx EXCEPT ![self] = cache_lock]
                                          /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                                   pc        |->  "cache_hit",
                                                                                   idx       |->  idx[self] ] >>
                                                                               \o stack[self]]
                                       /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                            ELSE /\ pc' = [pc EXCEPT ![self] = "cache_miss"]
                                 /\ UNCHANGED << stack, idx >>
                      /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                      wait_map, channels, tested_keys, idx_, 
                                      Key, local_wait_map >>

expiration_lock_1(self) == /\ pc[self] = "expiration_lock_1"
                           /\ cache' = [cache EXCEPT ![Key[self]] = FALSE]
                           /\ /\ idx' = [idx EXCEPT ![self] = cache_lock]
                              /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                       pc        |->  "expiration_done",
                                                                       idx       |->  idx[self] ] >>
                                                                   \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           wait_map, channels, tested_keys, 
                                           idx_, Key, local_wait_map >>

expiration_done(self) == /\ pc[self] = "expiration_done"
                         /\ pc' = [pc EXCEPT ![self] = "finished"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         cache, wait_map, channels, 
                                         tested_keys, stack, idx_, idx, Key, 
                                         local_wait_map >>

cache_hit(self) == /\ pc[self] = "cache_hit"
                   /\ pc' = [pc EXCEPT ![self] = "finished"]
                   /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                   wait_map, channels, tested_keys, stack, 
                                   idx_, idx, Key, local_wait_map >>

cache_miss(self) == /\ pc[self] = "cache_miss"
                    /\ tested_keys' = (tested_keys \union {Key[self]})
                    /\ /\ idx_' = [idx_ EXCEPT ![self] = wait_map_lock]
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                pc        |->  "lock_3",
                                                                idx_      |->  idx_[self] ] >>
                                                            \o stack[self]]
                    /\ pc' = [pc EXCEPT ![self] = "exchange"]
                    /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                    wait_map, channels, idx, Key, 
                                    local_wait_map >>

lock_3(self) == /\ pc[self] = "lock_3"
                /\ /\ idx' = [idx EXCEPT ![self] = cache_lock]
                   /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                            pc        |->  "lookup_wait_map",
                                                            idx       |->  idx[self] ] >>
                                                        \o stack[self]]
                /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                wait_map, channels, tested_keys, idx_, Key, 
                                local_wait_map >>

lookup_wait_map(self) == /\ pc[self] = "lookup_wait_map"
                         /\ IF Cardinality(wait_map[Key[self]]) > 0
                               THEN /\ pc' = [pc EXCEPT ![self] = "wait_map_subscribe"]
                               ELSE /\ pc' = [pc EXCEPT ![self] = "wait_map_register"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         cache, wait_map, channels, 
                                         tested_keys, stack, idx_, idx, Key, 
                                         local_wait_map >>

wait_map_subscribe(self) == /\ pc[self] = "wait_map_subscribe"
                            /\ wait_map' = [wait_map EXCEPT ![Key[self]] = wait_map[Key[self]] \union {self}]
                            /\ /\ idx' = [idx EXCEPT ![self] = wait_map_lock]
                               /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                        pc        |->  "waiting_for_value",
                                                                        idx       |->  idx[self] ] >>
                                                                    \o stack[self]]
                            /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                            /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                            cache, channels, tested_keys, idx_, 
                                            Key, local_wait_map >>

waiting_for_value(self) == /\ pc[self] = "waiting_for_value"
                           /\ channels[self]
                           /\ pc' = [pc EXCEPT ![self] = "finished"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           cache, wait_map, channels, 
                                           tested_keys, stack, idx_, idx, Key, 
                                           local_wait_map >>

wait_map_register(self) == /\ pc[self] = "wait_map_register"
                           /\ Assert(wait_map[Key[self]] = {}, 
                                     "Failure of assertion at line 145, column 17.")
                           /\ Assert(~cache[Key[self]], 
                                     "Failure of assertion at line 147, column 17.")
                           /\ wait_map' = [wait_map EXCEPT ![Key[self]] = {self}]
                           /\ /\ idx' = [idx EXCEPT ![self] = wait_map_lock]
                              /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                       pc        |->  "fetching_value",
                                                                       idx       |->  idx[self] ] >>
                                                                   \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                           /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                           cache, channels, tested_keys, idx_, 
                                           Key, local_wait_map >>

fetching_value(self) == /\ pc[self] = "fetching_value"
                        /\ TRUE
                        /\ pc' = [pc EXCEPT ![self] = "inserting_value_lock_cache"]
                        /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                        cache, wait_map, channels, tested_keys, 
                                        stack, idx_, idx, Key, local_wait_map >>

inserting_value_lock_cache(self) == /\ pc[self] = "inserting_value_lock_cache"
                                    /\ /\ idx_' = [idx_ EXCEPT ![self] = cache_lock]
                                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                                pc        |->  "inserting_value",
                                                                                idx_      |->  idx_[self] ] >>
                                                                            \o stack[self]]
                                    /\ pc' = [pc EXCEPT ![self] = "exchange"]
                                    /\ UNCHANGED << cache_lock, wait_map_lock, 
                                                    locks, cache, wait_map, 
                                                    channels, tested_keys, idx, 
                                                    Key, local_wait_map >>

inserting_value(self) == /\ pc[self] = "inserting_value"
                         /\ cache' = [cache EXCEPT ![Key[self]] = TRUE]
                         /\ /\ idx_' = [idx_ EXCEPT ![self] = wait_map_lock]
                            /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "lock",
                                                                     pc        |->  "remove_wait_map",
                                                                     idx_      |->  idx_[self] ] >>
                                                                 \o stack[self]]
                         /\ pc' = [pc EXCEPT ![self] = "exchange"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         wait_map, channels, tested_keys, idx, 
                                         Key, local_wait_map >>

remove_wait_map(self) == /\ pc[self] = "remove_wait_map"
                         /\ local_wait_map' = [local_wait_map EXCEPT ![self] = wait_map[Key[self]] \ { self }]
                         /\ wait_map' = [wait_map EXCEPT ![Key[self]] = {}]
                         /\ /\ idx' = [idx EXCEPT ![self] = wait_map_lock]
                            /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                                     pc        |->  "unlock_1",
                                                                     idx       |->  idx[self] ] >>
                                                                 \o stack[self]]
                         /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                         /\ UNCHANGED << cache_lock, wait_map_lock, locks, 
                                         cache, channels, tested_keys, idx_, 
                                         Key >>

unlock_1(self) == /\ pc[self] = "unlock_1"
                  /\ /\ idx' = [idx EXCEPT ![self] = cache_lock]
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "unlock",
                                                              pc        |->  "notify_all",
                                                              idx       |->  idx[self] ] >>
                                                          \o stack[self]]
                  /\ pc' = [pc EXCEPT ![self] = "reset_state"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, idx_, Key, 
                                  local_wait_map >>

notify_all(self) == /\ pc[self] = "notify_all"
                    /\ IF Cardinality(local_wait_map[self]) > 0
                          THEN /\ pc' = [pc EXCEPT ![self] = "notify"]
                          ELSE /\ pc' = [pc EXCEPT ![self] = "finished"]
                    /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                    wait_map, channels, tested_keys, stack, 
                                    idx_, idx, Key, local_wait_map >>

notify(self) == /\ pc[self] = "notify"
                /\ \E proc \in local_wait_map[self]:
                     /\ channels' = [channels EXCEPT ![proc] = TRUE]
                     /\ local_wait_map' = [local_wait_map EXCEPT ![self] = local_wait_map[self] \ {proc}]
                /\ pc' = [pc EXCEPT ![self] = "notify_all"]
                /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                wait_map, tested_keys, stack, idx_, idx, Key >>

finished(self) == /\ pc[self] = "finished"
                  /\ TRUE
                  /\ pc' = [pc EXCEPT ![self] = "Done"]
                  /\ UNCHANGED << cache_lock, wait_map_lock, locks, cache, 
                                  wait_map, channels, tested_keys, stack, idx_, 
                                  idx, Key, local_wait_map >>

P(self) == start(self) \/ cache_locked(self) \/ expiration_lock_1(self)
              \/ expiration_done(self) \/ cache_hit(self)
              \/ cache_miss(self) \/ lock_3(self) \/ lookup_wait_map(self)
              \/ wait_map_subscribe(self) \/ waiting_for_value(self)
              \/ wait_map_register(self) \/ fetching_value(self)
              \/ inserting_value_lock_cache(self) \/ inserting_value(self)
              \/ remove_wait_map(self) \/ unlock_1(self)
              \/ notify_all(self) \/ notify(self) \/ finished(self)

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
\* Last modified Wed Dec 15 23:25:36 CET 2021 by geal
\* Created Sat Dec 11 13:04:21 CET 2021 by geal

      
