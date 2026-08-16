// Status Panel Component - Shows daemon connection status

import React from "react";
import type { HealthStatus } from "../types";

interface Props {
  health: HealthStatus | null;
  loading: boolean;
  error: string | null;
}

export function StatusPanel({ health, loading, error }: Props) {
  if (loading && !health) {
    return (
      <div className="status-panel loading">
        <div className="spinner" />
        <span>Connecting to daemon...</span>
      </div>
    );
  }

  if (error && !health) {
    return (
      <div className="status-panel error">
        <div className="status-icon error">✕</div>
        <div className="status-info">
          <h3>Daemon Offline</h3>
          <p className="error-message">{error}</p>
          <p className="hint">
            Start the daemon with: <code>a3net daemon</code>
          </p>
        </div>
      </div>
    );
  }

  if (!health) return null;

  const statusClass = health.ok ? "healthy" : health.daemon_running ? "degraded" : "offline";

  return (
    <div className={`status-panel ${statusClass}`}>
      <div className={`status-icon ${statusClass}`}>
        {health.ok ? "✓" : health.daemon_running ? "⚠" : "✕"}
      </div>
      <div className="status-info">
        <h3>
          {health.ok ? "Connected" : health.daemon_running ? "Degraded" : "Offline"}
        </h3>
        {health.node_id && (
          <p className="node-id">
            Node: <code>{health.node_id.slice(0, 16)}...</code>
          </p>
        )}
        {health.latency_ms !== null && (
          <p className="latency">Latency: {health.latency_ms}ms</p>
        )}
        <p className="message">{health.message}</p>
      </div>
      <button className="refresh-btn" onClick={() => window.location.reload()}>
        ↻
      </button>
    </div>
  );
}
