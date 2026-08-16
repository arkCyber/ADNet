// React hooks for A3Net state management

import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import type { HealthStatus, NodeInfo, RoomFeed } from "../types";

// Hook for daemon health status
export function useHealth() {
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const status = await api.getHealth();
      setHealth(status);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to check health");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    // Poll every 5 seconds
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  return { health, loading, error, refresh };
}

// Hook for node information
export function useNodeInfo() {
  const [info, setInfo] = useState<NodeInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const nodeInfo = await api.getNodeInfo();
      setInfo(nodeInfo);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to get node info");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { info, loading, error, refresh };
}

// Hook for room feed
export function useRoomFeed(room: string | null) {
  const [feed, setFeed] = useState<RoomFeed | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!room) {
      setFeed(null);
      return;
    }

    let cancelled = false;

    async function fetchFeed() {
      setLoading(true);
      setError(null);
      try {
        const roomFeed = await api.getRoomFeed(room);
        if (!cancelled) {
          setFeed(roomFeed);
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to get room feed");
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    fetchFeed();
    // Poll every 3 seconds
    const interval = setInterval(fetchFeed, 3000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [room]);

  return { feed, loading, error };
}

// Hook for room list
export function useRooms() {
  const [rooms, setRooms] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const roomList = await api.listRooms();
      setRooms(roomList);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to list rooms");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { rooms, loading, error, refresh };
}
