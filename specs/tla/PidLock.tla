---------------------------- MODULE PidLock ----------------------------
(***************************************************************************)
(* Model of cartog's PID-file lock acquire protocol                        *)
(* (`crates/cartog-process-lock/src/lib.rs`, `ProcessLock::acquire`).      *)
(*                                                                         *)
(* WHAT THIS MODELS                                                        *)
(*  N processes race to claim a single lock slot. The on-disk file is a    *)
(*  shared resource. The protocol must guarantee:                          *)
(*    AtMostOneHolder       - never two live processes "hold" the slot.    *)
(*    LiveHolderOwnsItsFile - a live holder's on-disk file still records   *)
(*                            that holder's (pid, st); no acquirer wipes or *)
(*                            overwrites a live holder's file (the          *)
(*                            `unlink_if_unchanged` TOCTOU guard). Anchored *)
(*                            on the live holder so a clobber is a real     *)
(*                            violation, not a satisfied empty antecedent.  *)
(*                                                                         *)
(* THE LOAD-BEARING ASSUMPTION                                            *)
(*  `hard_link(tmp, target)` is ATOMIC: it either creates `target` (when   *)
(*  absent) or fails AlreadyExists (when present), no in-between state.     *)
(*  Modeled as the single atomic action `TryLink`. True on local POSIX     *)
(*  filesystems; NOT on NFS. If the deploy target is NFS this proof is      *)
(*  void - making that precondition explicit is the point of a spec.       *)
(*                                                                         *)
(*  `read`, the `unlink_if_unchanged` check, and `remove` are each their   *)
(*  own atomic step. The Rust does them as separate syscalls; one labeled  *)
(*  step each is what lets TLC interleave a crash or a competing writer     *)
(*  BETWEEN any two of them - the schedules a flaky test only samples.      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Procs            \* set of process ids, e.g. {1, 2, 3}

(* The file is Empty or a record [pid |-> p, st |-> s]. (pid, st) is the
   holder identity that `unlink_if_unchanged` re-checks. Each proc p uses
   st = p, so a re-created file is content-distinct from a stale one. *)
Empty == [pid |-> 0, st |-> 0]
IsEmpty(f) == f = Empty

\* The smallest process id seeds the race as the initial lock holder.
Owner == CHOOSE p \in Procs : \A q \in Procs : p <= q

(* --algorithm PidLock {
     variables
       \* Seed the race that the clobber bug needs: an INITIAL holder
       \* (the smallest proc id) already owns the slot. It may crash at any
       \* point, leaving a stale file; the other acquirers then read that
       \* stale holder and race to reap + re-create. This is the three-way
       \* TOCTOU (one holds-then-dies, one reaps+wins, one acts on its stale
       \* read) the `unlink_if_unchanged` guard exists to survive. Starting
       \* from Empty could never reach it: a winner here goes straight to
       \* `done`, so without a seeded holder no process ever reads a holder
       \* that later dies while a fresh writer lands in the gap.
       file = [pid |-> Owner, st |-> Owner],
       alive = [p \in Procs |-> TRUE],     \* is process p still running?
       holder = [p \in Procs |-> p = Owner]; \* the seeded owner holds at t0

     define {
       \* SAFETY 1: at most one process holds the slot at a time. This is the
       \* workhorse: removing the TOCTOU recheck at `unlinkIfUnchanged` lets a
       \* stale reaper wipe a fresh winner's file and then win the empty slot,
       \* producing two holders here (the 12-state counterexample).
       AtMostOneHolder ==
         \A p, q \in Procs : (holder[p] /\ holder[q]) => p = q

       \* SAFETY 2: a live holder's file must still record THAT holder's
       \* identity. The clobber bug's bad intermediate state is exactly its
       \* negation: process X believes it holds the slot (holder[X], alive[X]),
       \* but the on-disk file has been emptied or overwritten with a different
       \* (pid, st) by a concurrent reaper acting on a stale snapshot. Unlike a
       \* `FileHeldByLive => holder[file.pid]` phrasing (which a clobber
       \* trivially satisfies by setting file = Empty, making it vacuous), this
       \* is anchored on the LIVE HOLDER, so a wipe-out-from-under is a real
       \* violation, not a satisfied antecedent.
       LiveHolderOwnsItsFile ==
         \A p \in Procs :
           (holder[p] /\ alive[p]) =>
             (~IsEmpty(file) /\ file.pid = p /\ file.st = p)

       \* Combined invariant TLC checks.
       Safe == AtMostOneHolder /\ LiveHolderOwnsItsFile
     }

     \* Each acquirer runs the acquire loop. `self` is its pid. A crash is
     \* modeled as an `either` branch available at every step: the process
     \* may die instead of taking its next action, dropping its hold but
     \* leaving its (now stale) file on disk for another acquirer to reap.
     \* Folding crash INTO the process (rather than a separate process
     \* family) keeps process ids disjoint and models "dies at a point in
     \* its own execution" faithfully.
     fair process (proc \in Procs)
       variables seenPid = 0, seenSt = 0, seenAlive = FALSE;
     {
       \* TryLink: the atomic hard_link(tmp, target). One label = one atomic
       \* step, so a crash or a competing writer can be scheduled before it.
       tryLink:
         if (~alive[self]) { goto done; }
         else if (holder[self]) {
           \* Already the holder (the seeded owner at t0). It keeps running
           \* or crashes - a crash here is exactly what leaves a stale file
           \* for the other acquirers to race over. Without this branch the
           \* seeded owner would never die and the TOCTOU gap is unreachable.
           either { alive[self] := FALSE; holder[self] := FALSE; goto done; }
           or { goto done; }
         }
         else {
           either { alive[self] := FALSE; holder[self] := FALSE; goto done; }
           or if (IsEmpty(file)) {
                file := [pid |-> self, st |-> self];
                holder[self] := TRUE;
                goto done;                     \* won the slot (Ok)
              } else {
                goto readHolder;               \* AlreadyExists -> inspect
              };
         };
       \* Read the holder's recorded (pid, st) and whether it is alive.
       readHolder:
         either { alive[self] := FALSE; holder[self] := FALSE; goto done; }
         or if (IsEmpty(file)) { goto tryLink; }   \* holder's Drop unlinked it
            else {
              seenPid := file.pid;
              seenSt := file.st;
              seenAlive := alive[file.pid];       \* is_same_process(pid, st)
              goto inspect;
            };
       inspect:
         either { alive[self] := FALSE; holder[self] := FALSE; goto done; }
         or if (seenAlive) { goto done; }    \* live foreign holder -> Err(Held)
            else { goto unlinkIfUnchanged; }; \* stale -> reap then retry
       \* unlink_if_unchanged: remove ONLY IF current (pid, st) still equals
       \* what we read. A fresh writer may have re-created the file with a
       \* LIVE pid in the gap; the equality guard refuses to clobber it.
       unlinkIfUnchanged:
         either { alive[self] := FALSE; holder[self] := FALSE; goto done; }
         or {
           if (~IsEmpty(file) /\ file.pid = seenPid /\ file.st = seenSt) {
             file := Empty;
           };
           goto tryLink;                       \* retry the link
         };
       done: skip;
     }
   }
*)
\* BEGIN TRANSLATION (chksum(pcal) = "9b9e4180" /\ chksum(tla) = "46fe713")
VARIABLES file, alive, holder, pc

(* define statement *)
AtMostOneHolder ==
  \A p, q \in Procs : (holder[p] /\ holder[q]) => p = q










LiveHolderOwnsItsFile ==
  \A p \in Procs :
    (holder[p] /\ alive[p]) =>
      (~IsEmpty(file) /\ file.pid = p /\ file.st = p)


Safe == AtMostOneHolder /\ LiveHolderOwnsItsFile

VARIABLES seenPid, seenSt, seenAlive

vars == << file, alive, holder, pc, seenPid, seenSt, seenAlive >>

ProcSet == (Procs)

Init == (* Global variables *)
        /\ file = [pid |-> Owner, st |-> Owner]
        /\ alive = [p \in Procs |-> TRUE]
        /\ holder = [p \in Procs |-> p = Owner]
        (* Process proc *)
        /\ seenPid = [self \in Procs |-> 0]
        /\ seenSt = [self \in Procs |-> 0]
        /\ seenAlive = [self \in Procs |-> FALSE]
        /\ pc = [self \in ProcSet |-> "tryLink"]

tryLink(self) == /\ pc[self] = "tryLink"
                 /\ IF ~alive[self]
                       THEN /\ pc' = [pc EXCEPT ![self] = "done"]
                            /\ UNCHANGED << file, alive, holder >>
                       ELSE /\ IF holder[self]
                                  THEN /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                                             /\ holder' = [holder EXCEPT ![self] = FALSE]
                                             /\ pc' = [pc EXCEPT ![self] = "done"]
                                          \/ /\ pc' = [pc EXCEPT ![self] = "done"]
                                             /\ UNCHANGED <<alive, holder>>
                                       /\ file' = file
                                  ELSE /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                                             /\ holder' = [holder EXCEPT ![self] = FALSE]
                                             /\ pc' = [pc EXCEPT ![self] = "done"]
                                             /\ file' = file
                                          \/ /\ IF IsEmpty(file)
                                                   THEN /\ file' = [pid |-> self, st |-> self]
                                                        /\ holder' = [holder EXCEPT ![self] = TRUE]
                                                        /\ pc' = [pc EXCEPT ![self] = "done"]
                                                   ELSE /\ pc' = [pc EXCEPT ![self] = "readHolder"]
                                                        /\ UNCHANGED << file, 
                                                                        holder >>
                                             /\ alive' = alive
                 /\ UNCHANGED << seenPid, seenSt, seenAlive >>

readHolder(self) == /\ pc[self] = "readHolder"
                    /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                          /\ holder' = [holder EXCEPT ![self] = FALSE]
                          /\ pc' = [pc EXCEPT ![self] = "done"]
                          /\ UNCHANGED <<seenPid, seenSt, seenAlive>>
                       \/ /\ IF IsEmpty(file)
                                THEN /\ pc' = [pc EXCEPT ![self] = "tryLink"]
                                     /\ UNCHANGED << seenPid, seenSt, 
                                                     seenAlive >>
                                ELSE /\ seenPid' = [seenPid EXCEPT ![self] = file.pid]
                                     /\ seenSt' = [seenSt EXCEPT ![self] = file.st]
                                     /\ seenAlive' = [seenAlive EXCEPT ![self] = alive[file.pid]]
                                     /\ pc' = [pc EXCEPT ![self] = "inspect"]
                          /\ UNCHANGED <<alive, holder>>
                    /\ file' = file

inspect(self) == /\ pc[self] = "inspect"
                 /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                       /\ holder' = [holder EXCEPT ![self] = FALSE]
                       /\ pc' = [pc EXCEPT ![self] = "done"]
                    \/ /\ IF seenAlive[self]
                             THEN /\ pc' = [pc EXCEPT ![self] = "done"]
                             ELSE /\ pc' = [pc EXCEPT ![self] = "unlinkIfUnchanged"]
                       /\ UNCHANGED <<alive, holder>>
                 /\ UNCHANGED << file, seenPid, seenSt, seenAlive >>

unlinkIfUnchanged(self) == /\ pc[self] = "unlinkIfUnchanged"
                           /\ \/ /\ alive' = [alive EXCEPT ![self] = FALSE]
                                 /\ holder' = [holder EXCEPT ![self] = FALSE]
                                 /\ pc' = [pc EXCEPT ![self] = "done"]
                                 /\ file' = file
                              \/ /\ IF ~IsEmpty(file) /\ file.pid = seenPid[self] /\ file.st = seenSt[self]
                                       THEN /\ file' = Empty
                                       ELSE /\ TRUE
                                            /\ file' = file
                                 /\ pc' = [pc EXCEPT ![self] = "tryLink"]
                                 /\ UNCHANGED <<alive, holder>>
                           /\ UNCHANGED << seenPid, seenSt, seenAlive >>

done(self) == /\ pc[self] = "done"
              /\ TRUE
              /\ pc' = [pc EXCEPT ![self] = "Done"]
              /\ UNCHANGED << file, alive, holder, seenPid, seenSt, seenAlive >>

proc(self) == tryLink(self) \/ readHolder(self) \/ inspect(self)
                 \/ unlinkIfUnchanged(self) \/ done(self)

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == (\E self \in Procs: proc(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ \A self \in Procs : WF_vars(proc(self))

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 
=============================================================================
