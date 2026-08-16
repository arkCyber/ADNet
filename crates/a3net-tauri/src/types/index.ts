// Types matching the Rust backend responses

export interface HealthStatus {
  ok: boolean;
  daemon_running: boolean;
  node_id: string | null;
  latency_ms: number | null;
  message: string;
}

export interface NodeInfo {
  nodeId: string;
  dataDir: string | null;
  joinedRooms: string[];
  startedAt: string | null;
  mesh: MeshInfo | null;
  relay: RelayInfo | null;
}

export interface MeshInfo {
  host: string;
  port: number;
}

export interface RelayInfo {
  baseUrl: string;
  port: number;
}

export interface RoomFeed {
  room: string;
  assets: RoomAsset[];
  peerMap: Record<string, unknown>;
}

export interface RoomAsset {
  hash: string;
  title: string;
  kind: string;
  sizeBytes: number;
  mimeType: string | null;
  sourceUrl: string | null;
  announcerNodeId: string;
  announcedAt: string;
}

export interface SystemInfo {
  dataDir: string;
  daemon: DaemonStatus;
  node: NodeSummary | null;
  storage: StorageInfo | null;
}

export interface DaemonStatus {
  running: boolean;
  ipcSocket: string;
  uptimeSecs: number | null;
}

export interface NodeSummary {
  nodeId: string;
  shortId: string;
  peerCount: number;
  gossipTopics: number;
  joinedRooms: string[];
  meshHost: string | null;
  meshPort: number | null;
  relayUrl: string | null;
}

export interface StorageInfo {
  sharedBlobs: number;
  sharedBytes: number;
  privateBlobs: number;
  privateBytes: number;
  totalBytes: number;
}

export type RpcParams = Record<string, unknown>;
