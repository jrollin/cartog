--------------------------- MODULE Election ---------------------------
(***************************************************************************)
(* Model of cartog's single-writer election + promoter handoff            *)
(* (`crates/cartog-mcp/src/single_writer.rs`).                             *)
(*                                                                         *)
(* N `cartog serve` peers on the same DB elect one Primary via the O_EXCL  *)
(* serve lock; every other peer attaches ReadOnly and runs a promoter that *)
(* takes over if the Primary dies. The promoter sequence is:               *)
(*   poll primary liveness -> on death:                                    *)
(*     validate_pinned_state (PRE)  -> acquire (O_EXCL race) ->            *)
(*     validate_pinned_state (POST) -> open RW -> commit (flip to Primary).*)
(*                                                                         *)
(* SAFETY PROPERTIES                                                       *)
(*   AtMostOnePrimary - never two peers in the Primary role at once. Two   *)
(*                      Primaries = two RW writers on one DB.              *)
(*   PrimaryStateConsistent - a Primary never operates on a DB whose       *)
(*                      pinned state (schema_version, embedding            *)
(*                      fingerprint) drifted from what it validated. This  *)
(*                      is what the PRE/POST double-validate buys.         *)
(*                                                                         *)
(* ABSTRACTIONS                                                            *)
(*   The serve lock is modeled as an abstract test-and-set (`lockOwner`):  *)
(*   one winner, the rest see Held. PidLock.tla separately proves that     *)
(*   primitive race-free, so we take its atomicity as given here and focus *)
(*   on the promotion sequence and the schema-drift TOCTOU.                *)
(*                                                                         *)
(*   `validate_pinned_state` compares an attach-time snapshot (`pinned[p]`)*)
(*   against the live on-disk `schema`. A mismatch = "a third writer took  *)
(*   over and upgraded under us" -> abort promotion.                       *)
(*                                                                         *)
(*   A Primary may, while holding the lock, bump `schema` (a migration)    *)
(*   then exit. That is the event the POST-acquire re-validate must catch  *)
(*   in the window between a promoter's PRE-validate and its acquire.      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Procs,           \* set of peer ids, e.g. {1, 2, 3}
          MaxSchema        \* cap on migration count (state-space bound)

NoOwner == 0               \* sentinel: serve lock unheld

(* --algorithm Election {
     variables
       \* Abstract serve lock: 0 = unheld, else the holder's pid.
       lockOwner = NoOwner,
       \* On-disk pinned state (schema_version + embedding fingerprint,
       \* collapsed to one monotonically-bumped counter). A migration
       \* increments it.
       schema = 0,
       \* Role each peer believes it has. "elect" = pre-election start.
       role = [p \in Procs |-> "elect"],
       \* Liveness. A peer may crash at any step.
       alive = [p \in Procs |-> TRUE],
       \* Attach-time snapshot the promoter pins and re-checks. -1 = unset.
       pinned = [p \in Procs |-> 0-1];

     define {
       \* Only LIVE primaries matter: a crashed primary writes nothing, and
       \* its stale lock is reaped by the promoter. The danger is two LIVE
       \* RW writers on one DB.
       LivePrimaries == { p \in Procs : role[p] = "primary" /\ alive[p] }

       \* SAFETY 1: at most one live Primary at any time.
       AtMostOnePrimary == Cardinality(LivePrimaries) <= 1

       \* The lock and the Primary role agree: whoever is Primary holds the
       \* lock, and a held lock (by a live peer) means that peer is Primary.
       \* (A crashed Primary may transiently still "own" lockOwner until a
       \* promoter reaps it; we exclude the dead case.)
       LockMatchesPrimary ==
         \A p \in Procs :
           (role[p] = "primary" /\ alive[p]) => (lockOwner = p)

       \* SAFETY 2: every live Primary's pinned snapshot equals the on-disk
       \* schema. If a Primary ran with a stale snapshot, it would write
       \* vectors/edges against a schema it never reconciled - the drift
       \* the POST-acquire validate exists to prevent.
       PrimaryStateConsistent ==
         \A p \in Procs :
           (role[p] = "primary" /\ alive[p]) => (pinned[p] = schema)

       Safe == AtMostOnePrimary /\ LockMatchesPrimary /\ PrimaryStateConsistent

       \* State-space bound: cap the migration counter so TLC terminates.
       \* The migration loop is otherwise unbounded. Two migrations suffice
       \* to exercise the PRE-validate -> third-writer-migrates -> POST-
       \* validate drift window. Used as a CONSTRAINT in the .cfg.
       SchemaBound == schema <= MaxSchema
     }

     fair process (peer \in Procs)
     {
       \* --- Election: atomic O_EXCL acquire of the serve lock. ----------
       elect:
         either { alive[self] := FALSE; goto done; }
         or if (lockOwner = NoOwner) {
              lockOwner := self;
              pinned[self] := schema;       \* attach-time snapshot
              role[self] := "primary";
              goto primaryRun;
            } else {
              pinned[self] := schema;       \* read-only attach snapshot
              role[self] := "readonly";
              goto poll;
            };

       \* --- Primary main loop: may migrate (bump schema) then exit, or  --
       \* --- just exit, or crash. Exiting releases the lock (Drop).      --
       primaryRun:
         either { \* graceful exit: release lock so a reader can promote
           lockOwner := NoOwner;
           role[self] := "exited";
           goto done;
         }
         or { \* run a migration while holding the lock, then keep running
           schema := schema + 1;
           pinned[self] := schema;          \* our own snapshot stays current
           goto primaryRun;
         }
         or { alive[self] := FALSE; goto done; }   \* crash (lock NOT released)
         or { goto primaryRun; };                  \* idle

       \* --- ReadOnly promoter loop. -------------------------------------
       poll:
         either { alive[self] := FALSE; goto done; }
         or {
           \* Is the primary still alive? Modeled: lock still owned by a
           \* LIVE peer => primary present, keep polling.
           if (lockOwner # NoOwner /\ alive[lockOwner]) {
             goto poll;                      \* primary alive
           } else {
             goto preValidate;               \* primary gone (or crashed)
           };
         };

       \* Cheap pre-check before the lock acquire (validate_pinned_state #1).
       \* On match we proceed to acquire; the POST-acquire re-validate is what
       \* actually pins correctness (it re-checks after winning the lock), so
       \* this step has no carried state — it only gates the early abort below.
       preValidate:
         either { alive[self] := FALSE; goto done; }
         or if (pinned[self] = schema) {
              goto acquire;
            } else {
              \* state already diverged before we even tried: abort cleanly.
              role[self] := "aborted";
              goto done;
            };

       \* Atomic O_EXCL acquire. A crashed primary still "owns" lockOwner;
       \* the Rust reaps a stale PID file here. Model that as: acquire
       \* succeeds iff the current owner is gone (NoOwner) OR dead.
       acquire:
         either { alive[self] := FALSE; goto done; }
         or if (lockOwner = NoOwner \/ ~alive[lockOwner]) {
              lockOwner := self;             \* won (reaped any stale owner)
              goto postValidate;
            } else {
              \* another reader won the race / primary came back: stay RO.
              goto poll;
            };

       \* POST-acquire re-validate (validate_pinned_state #2). Between PRE
       \* and here a THIRD writer could have promoted, bumped schema, and
       \* exited (releasing the lock to us). We hold the lock now, so this
       \* check settles it: on drift, drop the lock and exit cleanly.
       postValidate:
         either { alive[self] := FALSE; goto done; }  \* crash: lock leaks, reaped later
         or if (pinned[self] = schema) {
              role[self] := "primary";       \* commit: flip role
              goto primaryRun;               \* now behave as a Primary
            } else {
              lockOwner := NoOwner;          \* drop the lock (drop new_lock)
              role[self] := "aborted";
              goto done;
            };

       done: skip;
     }
   }
*)
\* BEGIN TRANSLATION (chksum(pcal) = "26888784" /\ chksum(tla) = "8f7ee1df")
VARIABLES lockOwner, schema, role, alive, pinned, pc

(* define statement *)
LivePrimaries == { p \in Procs : role[p] = "primary" /\ alive[p] }


AtMostOnePrimary == Cardinality(LivePrimaries) <= 1





LockMatchesPrimary ==
  \A p \in Procs :
    (role[p] = "primary" /\ alive[p]) => (lockOwner = p)





PrimaryStateConsistent ==
  \A p \in Procs :
    (role[p] = "primary" /\ alive[p]) => (pinned[p] = schema)

Safe == AtMostOnePrimary /\ LockMatchesPrimary /\ PrimaryStateConsistent





SchemaBound == schema <= MaxSchema


vars == << lockOwner, schema, role, alive, pinned, pc >>

ProcSet == (Procs)

Init == (* Global variables *)
        /\ lockOwner = NoOwner
        /\ schema = 0
        /\ role = [p \in Procs |-> "elect"]
        /\ alive = [p \in Procs |-> TRUE]
        /\ pinned = [p \in Procs |-> 0-1]
        /\ pc = [self \in ProcSet |-> "elect"]

elect(self) == /\ pc[self] = "elect"
               /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = "done"]
                     /\ UNCHANGED <<lockOwner, role, pinned>>
                  \/ /\ IF lockOwner = NoOwner
                           THEN /\ lockOwner' = self
                                /\ pinned' = [pinned EXCEPT ![self] = schema]
                                /\ role' = [role EXCEPT ![self] = "primary"]
                                /\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                           ELSE /\ pinned' = [pinned EXCEPT ![self] = schema]
                                /\ role' = [role EXCEPT ![self] = "readonly"]
                                /\ pc' = [pc EXCEPT ![self] = "poll"]
                                /\ UNCHANGED lockOwner
                     /\ alive' = alive
               /\ UNCHANGED schema

primaryRun(self) == /\ pc[self] = "primaryRun"
                    /\ \/ /\ lockOwner' = NoOwner
                          /\ role' = [role EXCEPT ![self] = "exited"]
                          /\ pc' = [pc EXCEPT ![self] = "done"]
                          /\ UNCHANGED <<schema, alive, pinned>>
                       \/ /\ schema' = schema + 1
                          /\ pinned' = [pinned EXCEPT ![self] = schema']
                          /\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                          /\ UNCHANGED <<lockOwner, role, alive>>
                       \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                          /\ pc' = [pc EXCEPT ![self] = "done"]
                          /\ UNCHANGED <<lockOwner, schema, role, pinned>>
                       \/ /\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                          /\ UNCHANGED <<lockOwner, schema, role, alive, pinned>>

poll(self) == /\ pc[self] = "poll"
              /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                    /\ pc' = [pc EXCEPT ![self] = "done"]
                 \/ /\ IF lockOwner # NoOwner /\ alive[lockOwner]
                          THEN /\ pc' = [pc EXCEPT ![self] = "poll"]
                          ELSE /\ pc' = [pc EXCEPT ![self] = "preValidate"]
                    /\ alive' = alive
              /\ UNCHANGED << lockOwner, schema, role, pinned >>

preValidate(self) == /\ pc[self] = "preValidate"
                     /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                           /\ pc' = [pc EXCEPT ![self] = "done"]
                           /\ role' = role
                        \/ /\ IF pinned[self] = schema
                                 THEN /\ pc' = [pc EXCEPT ![self] = "acquire"]
                                      /\ role' = role
                                 ELSE /\ role' = [role EXCEPT ![self] = "aborted"]
                                      /\ pc' = [pc EXCEPT ![self] = "done"]
                           /\ alive' = alive
                     /\ UNCHANGED << lockOwner, schema, pinned >>

acquire(self) == /\ pc[self] = "acquire"
                 /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                       /\ pc' = [pc EXCEPT ![self] = "done"]
                       /\ UNCHANGED lockOwner
                    \/ /\ IF lockOwner = NoOwner \/ ~alive[lockOwner]
                             THEN /\ lockOwner' = self
                                  /\ pc' = [pc EXCEPT ![self] = "postValidate"]
                             ELSE /\ pc' = [pc EXCEPT ![self] = "poll"]
                                  /\ UNCHANGED lockOwner
                       /\ alive' = alive
                 /\ UNCHANGED << schema, role, pinned >>

postValidate(self) == /\ pc[self] = "postValidate"
                      /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                            /\ pc' = [pc EXCEPT ![self] = "done"]
                            /\ UNCHANGED <<lockOwner, role>>
                         \/ /\ IF pinned[self] = schema
                                  THEN /\ role' = [role EXCEPT ![self] = "primary"]
                                       /\ pc' = [pc EXCEPT ![self] = "primaryRun"]
                                       /\ UNCHANGED lockOwner
                                  ELSE /\ lockOwner' = NoOwner
                                       /\ role' = [role EXCEPT ![self] = "aborted"]
                                       /\ pc' = [pc EXCEPT ![self] = "done"]
                            /\ alive' = alive
                      /\ UNCHANGED << schema, pinned >>

done(self) == /\ pc[self] = "done"
              /\ TRUE
              /\ pc' = [pc EXCEPT ![self] = "Done"]
              /\ UNCHANGED << lockOwner, schema, role, alive, pinned >>

peer(self) == elect(self) \/ primaryRun(self) \/ poll(self)
                 \/ preValidate(self) \/ acquire(self)
                 \/ postValidate(self) \/ done(self)

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == (\E self \in Procs: peer(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ \A self \in Procs : WF_vars(peer(self))

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 
=============================================================================
