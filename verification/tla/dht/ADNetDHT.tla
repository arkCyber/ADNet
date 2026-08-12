------------------------ MODULE ADNetDHT ------------------------
(*
 * ADNet DHT - Kademlia-style Distributed Hash Table
 *
 * Models a 4-node, K-bucket bounded Kademlia routing table. The state
 * captures the current node set and the per-node routing table. Node
 * join/leave is the only mutation exercised under the bounded
 * `STATE CONSTRAINT <= 4` from `DHT.cfg`. Key/value storage is
 * intentionally abstracted away -- this spec isolates the routing
 * invariants (I1-I4) and a single safety/liveness property (L1).
 *
 * The model is intentionally small (4 nodes, k=3, alpha=3) so that
 * TLC-2 can exhaust the reachable state space in a few seconds. The
 * alias `NodeID` is provided so that the cfg file's `NodeID = {0,1,2,3}`
 * symbolic set is honoured.
 *)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    ID,        \* 0..15 ID space (declared in DHT.cfg)
    NodeID,    \* Concrete node set used by the cfg (size 4)
    Key,       \* Abstract key set (size 3)
    Value,     \* Abstract value set (size 3)
    K,         \* Maximum bucket size (k=3)
    Alpha      \* Concurrency parameter (alpha=3)

ASSUME
    /\ NodeID \subseteq ID
    /\ NodeID = {0, 1, 2, 3}

VARIABLES
    nodes,    \* Set of currently joined nodes, subset of NodeID
    table     \* Function [NodeID -> SUBSET NodeID]; the routing table
              \* of each node (the union of its k-buckets). A node
              \* never lists itself in its own table.

vars == <<nodes, table>>

----------------------------------------------------------------------
\* Helpers
----------------------------------------------------------------------

Peers(n) == {m \in NodeID : m # n}

----------------------------------------------------------------------
\* Type / well-formedness
----------------------------------------------------------------------

TypeOK ==
    /\ nodes \subseteq NodeID
    /\ table \in [NodeID -> SUBSET NodeID]

----------------------------------------------------------------------
\* Initial state
----------------------------------------------------------------------

Init ==
    /\ nodes = {}
    /\ table = [n \in NodeID |-> {}]

----------------------------------------------------------------------
\* Actions
----------------------------------------------------------------------

\* Add a node that is not currently joined.
AddNode(n) ==
    /\ n \in NodeID
    /\ n \notin nodes
    /\ nodes' = nodes \cup {n}
    /\ table' = [t \in NodeID |->
        IF t = n
            THEN {}\* new node starts with empty table
            ELSE\* every existing node adds n to its table (capped to K)
                IF n \in table[t] THEN table[t]
                ELSE IF Cardinality(table[t]) < K
                    THEN table[t] \cup {n}
                    ELSE table[t]]  \* saturated -- don't evict

\* Remove a node that is currently joined.
RemoveNode(n) ==
    /\ n \in nodes
    /\ nodes' = nodes \ {n}
    /\ table' = [t \in NodeID |->
        IF n \in table[t] THEN table[t] \ {n} ELSE table[t]]

Next ==
    \/ \E n \in NodeID: AddNode(n)
    \/ \E n \in NodeID: RemoveNode(n)

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------
\* Invariants
----------------------------------------------------------------------

\* I1: Nodes is always a subset of NodeID (no ghost nodes).
I1 == nodes \subseteq NodeID

\* I2: No node lists itself in its own routing table.
I2 ==
    \A n \in NodeID:
        n \notin table[n]

\* I3: Every entry in any node's table is a currently joined node.
I3 ==
    \A n \in NodeID:
        \A x \in table[n]:
            x \in nodes

\* I4: Bucket size (singleton "table" view) is bounded by K.
I4 ==
    \A n \in NodeID:
        Cardinality(table[n]) <= K

----------------------------------------------------------------------
\* Safety / liveness
----------------------------------------------------------------------

\* L1: Node set is bounded by the configured NodeID set.
L1 == [] (Cardinality(nodes) <= 4)

----------------------------------------------------------------------
\* Named constraint (used by the cfg's CONSTRAINT clause).
\* Defined as an operator because `CONSTRAINT` in a TLC cfg file
\* must reference a name defined in the spec (not an inline
\* expression).
----------------------------------------------------------------------

Constraint == Cardinality(nodes) <= 4

NextConstraint == \E n \in ID \ nodes: AddNode(n)

=============================================================================
\* Modification History
\* Last modified Mon Aug 12 16:00:00 UTC 2026
