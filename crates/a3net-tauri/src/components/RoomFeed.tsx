// Room Feed Component - Shows assets in a room

import React, { useState } from "react";
import type { RoomFeed } from "../types";
import { formatBytes, formatTime } from "../utils";

interface Props {
  room: string;
  feed: RoomFeed | null;
  loading: boolean;
  error: string | null;
  onJoin: (room: string) => void;
  onLeave: (room: string) => void;
  joinedRooms: string[];
}

export function RoomFeedPanel({
  room,
  feed,
  loading,
  error,
  onJoin,
  onLeave,
  joinedRooms,
}: Props) {
  const [inputRoom, setInputRoom] = useState("");
  const isJoined = joinedRooms.includes(room);

  const handleJoin = () => {
    if (inputRoom.trim()) {
      onJoin(inputRoom.trim());
      setInputRoom("");
    }
  };

  return (
    <div className="room-feed-panel">
      <div className="room-header">
        <h2>Room: {room}</h2>
        {isJoined ? (
          <button className="btn leave" onClick={() => onLeave(room)}>
            Leave
          </button>
        ) : (
          <button className="btn join" onClick={() => onJoin(room)}>
            Join
          </button>
        )}
      </div>

      <div className="room-join-form">
        <input
          type="text"
          placeholder="Enter room name..."
          value={inputRoom}
          onChange={(e) => setInputRoom(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleJoin()}
        />
        <button className="btn join" onClick={handleJoin}>
          Join Room
        </button>
      </div>

      {loading && (
        <div className="loading-state">
          <div className="spinner" />
          <span>Loading feed...</span>
        </div>
      )}

      {error && (
        <div className="error-state">
          <span className="error-icon">⚠</span>
          <span>{error}</span>
        </div>
      )}

      {feed && !loading && (
        <div className="feed-content">
          <div className="feed-stats">
            <span>{feed.assets.length} assets</span>
            <span>{Object.keys(feed.peerMap || {}).length} peers</span>
          </div>

          {feed.assets.length === 0 ? (
            <div className="empty-state">
              <span>No assets in this room yet.</span>
              <span className="hint">
                Announce a file to share it with the room.
              </span>
            </div>
          ) : (
            <div className="asset-list">
              {feed.assets.map((asset) => (
                <div key={asset.hash} className="asset-item">
                  <div className="asset-icon">
                    {asset.kind === "video" ? "🎬" :
                     asset.kind === "audio" ? "🎵" :
                     asset.kind === "image" ? "🖼" : "📄"}
                  </div>
                  <div className="asset-info">
                    <span className="asset-title">{asset.title}</span>
                    <span className="asset-meta">
                      {formatBytes(asset.sizeBytes)} • {asset.kind}
                    </span>
                    <span className="asset-time">
                      {formatTime(asset.announcedAt)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
