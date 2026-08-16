// API hooks for calling Tauri commands via HTTP RPC

import { invoke } from "@tauri-apps/api/core";
import type {
  HealthStatus,
  NodeInfo,
  RoomFeed,
  SystemInfo,
} from "../types";

const RPC_URL = "http://127.0.0.1:11436/rpc";

interface RpcRequest {
  jsonrpc: "2.0";
  method: string;
  params: Record<string, unknown>;
  id: number;
}

interface RpcResponse {
  jsonrpc: "2.0";
  result?: unknown;
  error?: { code: number; message: string };
  id: number;
}

async function rpcCall<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  const response = await fetch(RPC_URL, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      method,
      params,
      id: 1,
    } as RpcRequest),
  });

  if (!response.ok) {
    throw new Error(`HTTP error: ${response.status}`);
  }

  const data = (await response.json()) as RpcResponse;

  if (data.error) {
    throw new Error(`RPC error ${data.error.code}: ${data.error.message}`);
  }

  return data.result as T;
}

// Tauri command wrappers
export async function getHealth(): Promise<HealthStatus> {
  return invoke<HealthStatus>("get_health");
}

export async function getSystemInfo(): Promise<SystemInfo> {
  return invoke<SystemInfo>("get_system_info");
}

export async function getNodeInfo(): Promise<NodeInfo> {
  return rpcCall<NodeInfo>("info", {});
}

export async function listRooms(): Promise<string[]> {
  return rpcCall<string[]>("list_rooms", {});
}

export async function joinRoom(room: string): Promise<void> {
  await rpcCall("join", { room });
}

export async function leaveRoom(room: string): Promise<void> {
  await rpcCall("leave", { room });
}

export async function getRoomFeed(room: string): Promise<RoomFeed> {
  return rpcCall<RoomFeed>("feed", { room });
}

export async function checkDaemonReachable(): Promise<boolean> {
  try {
    const response = await fetch("http://127.0.0.1:11436/health");
    return response.ok;
  } catch {
    return false;
  }
}

export { RPC_URL };
