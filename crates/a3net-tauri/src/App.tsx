import React, { useState, useCallback } from "react";
import { StatusPanel } from "./components/StatusPanel";
import { RoomFeedPanel } from "./components/RoomFeed";
import { useHealth, useRooms, useRoomFeed } from "./hooks";
import * as api from "./api";

export default function App() {
  const { health, loading: healthLoading, error: healthError } = useHealth();
  const { rooms, loading: roomsLoading, refresh: refreshRooms } = useRooms();
  const [selectedRoom, setSelectedRoom] = useState<string>("lobby");
  const { feed, loading: feedLoading, error: feedError } = useRoomFeed(
    health?.daemon_running ? selectedRoom : null
  );

  const handleJoinRoom = useCallback(
    async (room: string) => {
      try {
        await api.joinRoom(room);
        await refreshRooms();
        setSelectedRoom(room);
      } catch (e) {
        console.error("Failed to join room:", e);
      }
    },
    [refreshRooms]
  );

  const handleLeaveRoom = useCallback(
    async (room: string) => {
      try {
        await api.leaveRoom(room);
        await refreshRooms();
      } catch (e) {
        console.error("Failed to leave room:", e);
      }
    },
    [refreshRooms]
  );

  return (
    <div className="app">
      <header className="app-header">
        <h1>A3Net</h1>
        <div className="rpc-info">
          Connected to: <code>{api.RPC_URL}</code>
        </div>
      </header>

      <main className="app-main">
        <aside className="sidebar">
          <h2>Rooms</h2>
          <ul className="room-list">
            {roomsLoading ? (
              <li className="loading">Loading...</li>
            ) : (
              rooms.map((room) => (
                <li
                  key={room}
                  className={selectedRoom === room ? "active" : ""}
                  onClick={() => setSelectedRoom(room)}
                >
                  {room}
                </li>
              ))
            )}
          </ul>
        </aside>

        <section className="content">
          <StatusPanel
            health={health}
            loading={healthLoading}
            error={healthError}
          />

          {health?.daemon_running && (
            <RoomFeedPanel
              room={selectedRoom}
              feed={feed}
              loading={feedLoading}
              error={feedError}
              onJoin={handleJoinRoom}
              onLeave={handleLeaveRoom}
              joinedRooms={rooms}
            />
          )}
        </section>
      </main>
    </div>
  );
}
