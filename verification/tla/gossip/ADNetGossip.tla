------------------------ MODULE ADNetGossip ------------------------
(* ADNet Gossip Protocol - Epidemic Broadcast *)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Node,
    MsgId,
    Fanout,
    MaxHop,
    BufferSize

VARIABLES
    connected,
    sent,
    received,
    pending,
    delivered,
    clock,
    membership

TypeOK ==
    /\ connected \in [Node -> SUBSET Node]
    /\ sent \in [Node -> SUBSET MsgId]
    /\ received \in [Node -> SUBSET MsgId]
    /\ pending \in [Node -> Seq(MSG)]
    /\ delivered \in [Node -> Seq(MSG)]
    /\ clock \in [Node -> [Node -> Nat]]
    /\ membership \in [Node -> SUBSET Node]

MSG == [
    id: MsgId,
    data: STRING,
    sender: Node,
    hop: 0..MaxHop,
    ts: [Node -> Nat]
]

Init ==
    /\ connected = [n \in Node |-> {}]
    /\ sent = [n \in Node |-> {}]
    /\ received = [n \in Node |-> {}]
    /\ pending = [n \in Node |-> << >>]
    /\ delivered = [n \in Node |-> << >>]
    /\ clock = [n \in Node |-> [m \in Node |-> 0]]
    /\ membership = [n \in Node |-> {n}]

Connect(n, m) ==
    /\ n # m
    /\ connected' = [connected EXCEPT ![n] = @ \cup {m}, ![m] = @ \cup {n}]
    /\ membership' = [membership EXCEPT ![n] = @ \cup {m}, ![m] = @ \cup {n}]
    /\ UNCHANGED <<sent, received, pending, delivered, clock>>

Disconnect(n, m) ==
    /\ m \in connected[n]
    /\ connected' = [connected EXCEPT ![n] = @ \ {m}, ![m] = @ \ {n}]
    /\ UNCHANGED <<sent, received, pending, delivered, clock, membership>>

Publish(n, mid, data) ==
    /\ mid \notin sent[n]
    /\ sent' = [sent EXCEPT ![n] = @ \cup {mid}]
    /\ received' = [received EXCEPT ![n] = @ \cup {mid}]
    /\ pending' = [pending EXCEPT ![n] = Append(@,
        [id |-> mid, data |-> data, sender |-> n, hop |-> 0,
         ts |-> [clock[n] EXCEPT ![n] = @ + 1]])]
    /\ clock' = [clock EXCEPT ![n] = [m \in Node |->
        IF m = n THEN @ + 1 ELSE @]]
    /\ UNCHANGED <<connected, delivered, membership>>

Push(n) ==
    /\ pending[n] # << >>
    /\ \E target \in connected[n]:
        LET msg == Head(pending[n])
        IN /\ msg.hop < MaxHop
           /\ sent' = [sent EXCEPT ![n] = @ \cup {msg.id}]
           /\ pending' = [pending EXCEPT ![n] = 
               Append(Tail(@), 
                   [id |-> msg.id, data |-> msg.data, sender |-> n,
                    hop |-> msg.hop + 1, ts |-> msg.ts])]
    /\ UNCHANGED <<connected, received, delivered, clock, membership>>

Deliver(n) ==
    /\ \E i \in 1..Len(pending[n]):
        LET msg == pending[n][i]
        IN /\ msg.id \notin received[n]
           /\ received' = [received EXCEPT ![n] = @ \cup {msg.id}]
           /\ delivered' = [delivered EXCEPT ![n] = Append(@, msg)]
           /\ pending' = [pending EXCEPT ![n] = 
               [j \in 1..Len(@) |-> IF j = i THEN @[j] ELSE 
                   IF j < i THEN @[j] ELSE @[j + 1]]]
           /\ clock' = [clock EXCEPT ![n] = 
               [m \in Node |->
                   IF m = msg.sender THEN @ + 1 
                   ELSE MAX(@, msg.ts[m])]]
    /\ UNCHANGED <<connected, sent, membership>>

DeliverDup(n) ==
    /\ \E i \in 1..Len(pending[n]):
        LET msg == pending[n][i]
        IN /\ msg.id \in received[n]
           /\ pending' = [pending EXCEPT ![n] = 
               [j \in 1..Len(@) |-> IF j = i THEN @[j] ELSE 
                   IF j < i THEN @[j] ELSE @[j + 1]]]
    /\ UNCHANGED <<connected, sent, received, delivered, clock, membership>>

AntiEntropy(n, m) ==
    /\ n \in Node /\ m \in Node /\ n # m
    /\ m \in connected[n]
    /\ sent' = [sent EXCEPT ![n] = @ \cup sent[m], ![m] = @ \cup sent[n]]
    /\ received' = [received EXCEPT ![n] = @ \cup received[m], ![m] = @ \cup received[n]]
    /\ delivered' = [delivered EXCEPT ![n] = @ \cup delivered[m], ![m] = @ \cup delivered[n]]
    /\ pending' = [pending EXCEPT ![n] = @ \cup pending[m], ![m] = @ \cup pending[n]]
    /\ UNCHANGED <<connected, clock, membership>>

Next ==
    \/ \E n, m \in Node: Connect(n, m)
    \/ \E n, m \in Node: Disconnect(n, m)
    \/ \E n \in Node, mid \in MsgId, d \in STRING: Publish(n, mid, d)
    \/ \E n \in Node: Push(n)
    \/ \E n \in Node: Deliver(n)
    \/ \E n \in Node: DeliverDup(n)
    \/ \E n, m \in Node: AntiEntropy(n, m)

Spec == Init /\ [][Next]_<<connected, sent, received, pending, delivered, clock, membership>>

Inv1 ==
    \A n \in Node, m \in connected[n]: m \in connected[m]

Inv2 ==
    \A n \in Node:
        \A i, j \in 1..Len(delivered[n]):
            i # j => delivered[n][i].id # delivered[n][j].id

Inv3 ==
    \A n \in Node:
        \A i, j \in 1..Len(delivered[n]):
            i < j =>
                \A m \in Node:
                    delivered[n][i].ts[m] <= delivered[n][j].ts[m]

L1 ==
    \A n \in Node, mid \in MsgId:
        (mid \in sent[n]) ~> (\A m \in Node: mid \in {d.id: d \in delivered[m]})
=============================================================================
