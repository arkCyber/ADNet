------------------------ MODULE ADNetBitswap ------------------------
(* ADNet Bitswap Protocol - Content Exchange *)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Node,
    Block,
    CID,
    MaxDebt

VARIABLES
    blocks,
    wantlist,
    have,
    ledger,
    pending,
    sent,
    debt

TypeOK ==
    /\ blocks \in [Node -> SUBSET Block]
    /\ wantlist \in [Node -> SUBSET CID]
    /\ have \in [Node -> SUBSET CID]
    /\ ledger \in [Node -> [Node -> [
        sent: Nat,
        received: Nat,
        last: Nat
    ]]]
    /\ pending \in [Node -> SUBSET CID]
    /\ sent \in [Node -> SUBSET CID]
    /\ debt \in [Node -> [Node -> Nat]]

Init ==
    /\ blocks = [n \in Node |-> {}]
    /\ wantlist = [n \in Node |-> {}]
    /\ have = [n \in Node |-> {}]
    /\ ledger = [n \in Node |->
        [m \in Node |->
            [sent |-> 0, received |-> 0, last |-> 0]]]
    /\ pending = [n \in Node |-> {}]
    /\ sent = [n \in Node |-> {}]
    /\ debt = [n \in Node |->
        [m \in Node |-> 0]]

AddBlock(n, b) ==
    /\ b \notin blocks[n]
    /\ blocks' = [blocks EXCEPT ![n] = @ \cup {b}]
    /\ have' = [have EXCEPT ![n] = @ \cup {b}]
    /\ UNCHANGED <<wantlist, ledger, pending, sent, debt>>

RemoveBlock(n, b) ==
    /\ b \in blocks[n]
    /\ blocks' = [blocks EXCEPT ![n] = @ \ {b}]
    /\ UNCHANGED <<wantlist, have, ledger, pending, sent, debt>>

Want(n, c) ==
    /\ c \notin wantlist[n]
    /\ c \notin have[n]
    /\ wantlist' = [wantlist EXCEPT ![n] = @ \cup {c}]
    /\ pending' = [pending EXCEPT ![n] = @ \cup {c}]
    /\ UNCHANGED <<blocks, have, ledger, sent, debt>>

CancelWant(n, c) ==
    /\ c \in wantlist[n]
    /\ wantlist' = [wantlist EXCEPT ![n] = @ \ {c}]
    /\ pending' = [pending EXCEPT ![n] = @ \ {c}]
    /\ UNCHANGED <<blocks, have, ledger, sent, debt>>

SendBlock(src, dst, c) ==
    /\ src \in Node /\ dst \in Node
    /\ src # dst
    /\ c \in have[src]
    /\ c \notin sent[src]
    /\ sent' = [sent EXCEPT ![src] = @ \cup {c}]
    /\ debt' = [debt EXCEPT ![dst][src] = @ + 1]
    /\ ledger' = [ledger EXCEPT 
        ![src][dst] = [@ EXCEPT !.sent = @ + 1, !.last = @ + 1]]
    /\ UNCHANGED <<blocks, wantlist, have, pending>>

ReceiveBlock(dst, src, c) ==
    /\ src \in Node /\ dst \in Node
    /\ src # dst
    /\ c \in sent[src]
    /\ c \in pending[dst]
    /\ have' = [have EXCEPT ![dst] = @ \cup {c}]
    /\ pending' = [pending EXCEPT ![dst] = @ \ {c}]
    /\ wantlist' = [wantlist EXCEPT ![dst] = 
        IF c \in @ THEN @ \ {c} ELSE @]
    /\ ledger' = [ledger EXCEPT 
        ![dst][src] = [@ EXCEPT !.received = @ + 1, !.last = @ + 1]]
    /\ debt' = [debt EXCEPT ![dst][src] = @ - 1]
    /\ UNCHANGED <<blocks, sent>>

BlockDueToDebt(n, m) ==
    /\ debt[n][m] >= MaxDebt
    /\ debt' = [debt EXCEPT ![n][m] = @ + 1]
    /\ UNCHANGED <<blocks, wantlist, have, pending, sent, ledger>>

ResetLedger(n, m) ==
    /\ ledger' = [ledger EXCEPT 
        ![n][m] = [sent |-> 0, received |-> 0, last |-> 0]]
    /\ debt' = [debt EXCEPT ![n][m] = 0]
    /\ UNCHANGED <<blocks, wantlist, have, pending, sent>>

Next ==
    \/ \E n \in Node, b \in Block: AddBlock(n, b)
    \/ \E n \in Node, b \in Block: RemoveBlock(n, b)
    \/ \E n \in Node, c \in CID: Want(n, c)
    \/ \E n \in Node, c \in CID: CancelWant(n, c)
    \/ \E s \in Node, d \in Node, c \in CID: SendBlock(s, d, c)
    \/ \E d \in Node, s \in Node, c \in CID: ReceiveBlock(d, s, c)
    \/ \E n \in Node, m \in Node: ResetLedger(n, m)

Spec == Init /\ [][Next]_<<blocks, wantlist, have, ledger, pending, sent, debt>>

(* Invariants *)

Inv1 ==
    \A n \in Node:
        \A c \in wantlist[n]: 
            c \notin have[n]

Inv2 ==
    \A n \in Node:
        \A c \in pending[n]:
            c \in wantlist[n]

Inv3 ==
    \A n \in Node, m \in Node \ {n}:
        ledger[n][m].received <= ledger[n][m].sent + 1

Inv4 ==
    \A n \in Node, m \in Node:
        debt[n][m] >= 0

(* Liveness *)

L1 ==
    \A n \in Node, c \in wantlist[n]:
        <> (c \in have[n])

L2 ==
    \A s \in Node, d \in Node, c \in sent[s]:
        (c \in have[s] /\ c \in wantlist[d]) ~> (c \in have[d])
=============================================================================
