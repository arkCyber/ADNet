# A3Net Tauri Desktop UI

A cross-platform desktop UI for A3Net, built with Tauri 2 + React + TypeScript.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    A3Net Tauri Desktop UI                       │
│                                                                 │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐ │
│  │   React UI      │◄─┤  Tauri Bridge   ├─►│  Rust Backend   │ │
│  │  (TypeScript)   │  │   (IPC Layer)   │  │  (Commands)     │ │
│  └─────────────────┘  └─────────────────┘  └────────┬────────┘ │
│                                                       │          │
└───────────────────────────────────────────────────────┼──────────┘
                                                        │
                                                        ▼
                            ┌─────────────────────────────────────────────┐
                            │       A3Net Daemon                         │
                            │       HTTP RPC (127.0.0.1:11436)          │
                            └─────────────────────────────────────────────┘
```

## Directory Structure

```
crates/a3net-tauri/
├── src-tauri/              # Rust backend
│   ├── src/
│   │   ├── main.rs        # Binary entry point
│   │   └── lib.rs         # Tauri commands & run() function
│   ├── capabilities/
│   │   └── default.json   # Security capabilities
│   ├── icons/
│   │   └── icon.png       # App icon
│   ├── build.rs           # Build script (tauri-build)
│   ├── Cargo.toml         # Rust dependencies
│   └── tauri.conf.json    # Tauri configuration
├── src/                   # React frontend
│   ├── App.tsx            # Main app component
│   ├── main.tsx           # React entry point
│   ├── styles.css         # Global styles
│   ├── api/
│   │   └── index.ts       # HTTP RPC client
│   ├── components/
│   │   ├── StatusPanel.tsx
│   │   └── RoomFeed.tsx
│   ├── hooks/
│   │   └── index.ts       # React hooks for state management
│   ├── types/
│   │   └── index.ts       # TypeScript type definitions
│   └── utils/
│       └── index.ts       # Utility functions
├── public/
│   └── icon.svg           # SVG icon
├── dist/                  # Build output (generated)
├── index.html             # HTML entry point
├── package.json           # NPM dependencies
├── tsconfig.json          # TypeScript config
└── vite.config.ts         # Vite bundler config
```

## Features

- **Daemon Health Monitor**: Real-time health status with 5-second polling
- **Room Management**: List, join, and leave rooms
- **Room Feed Viewer**: View shared assets in joined rooms
- **Raw RPC Interface**: Call any JSON-RPC method on the daemon
- **Cross-Platform**: Windows, macOS, Linux via Tauri

## Setup

### Prerequisites

- Node.js 18+ and npm
- Rust 1.75+ and Cargo
- A3Net daemon running locally (port 11436)

### Install Dependencies

```bash
cd crates/a3net-tauri
npm install
```

### Development

```bash
# Terminal 1: Start the A3Net daemon
a3net daemon

# Terminal 2: Start the Tauri app in dev mode
cd crates/a3net-tauri
npm run tauri dev
```

### Build for Production

```bash
cd crates/a3net-tauri
npm run tauri build
```

## Tauri Commands (Rust → JS Bridge)

The Rust backend exposes these Tauri commands:

| Command | Description |
|---------|-------------|
| `get_health` | Get daemon health status |
| `get_system_info` | Get full system info |
| `get_node_info` | Get node identity |
| `list_rooms` | List joined rooms |
| `join_room` | Join a room |
| `leave_room` | Leave a room |
| `get_room_feed` | Get room feed |
| `rpc_call` | Call raw RPC method |
| `get_rpc_url` | Get RPC URL |
| `check_daemon_reachable` | Check if daemon is up |

## HTTP RPC Endpoint

The Tauri app connects to the A3Net daemon via:

```
http://127.0.0.1:11436/rpc
```

This is the daemon's HTTP RPC interface (added in the `a3net-ipc-adapter` crate).

## Security

- The Tauri webview has CSP enabled restricting connections to localhost only
- All HTTP calls are made to `127.0.0.1:11436` (loopback only)
- Bearer token authentication supported by the daemon
