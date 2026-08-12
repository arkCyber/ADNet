# ADNet AI Model Distribution Network

## Overview

This document describes the AI Model Distribution Network built on top of ADNet's P2P infrastructure. It enables decentralized distribution of AI models using Iroh's blob transfer protocol.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Model Distribution Network                       │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────────────┐         ┌──────────────────┐                      │
│  │   Model Provider │         │   Model Provider │                      │
│  │   (Your NAS)     │         │   (Community)    │                      │
│  │                  │         │                  │                      │
│  │  ┌────────────┐  │         │  ┌────────────┐  │                      │
│  │  │ Blob Store │  │         │  │ Blob Store │  │                      │
│  │  │ (Models)   │  │         │  │ (Models)   │  │                      │
│  │  └────────────┘  │         │  └────────────┘  │                      │
│  │         │        │         │         │        │                      │
│  │         ▼        │         │         ▼        │                      │
│  │  ┌────────────┐  │         │  ┌────────────┐  │                      │
│  │  │  Catalog   │  │         │  │  Catalog   │  │                      │
│  │  │  Index     │  │         │  │  Index     │  │                      │
│  │  └────────────┘  │         │  └────────────┘  │                      │
│  └────────┬─────────┘         └────────┬─────────┘                      │
│           │                             │                                 │
│           │    ┌─────────────────────┐  │                                 │
│           │    │  Gossip Bus         │  │                                 │
│           │    │  (Model Discovery)  │◄─┤                                 │
│           │    └─────────────────────┘  │                                 │
│           │              │              │                                 │
│           ▼              ▼              ▼                                 │
│  ┌──────────────────────────────────────────────────────────┐             │
│  │                  P2P Network (Iroh)                     │             │
│  │   ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐ │             │
│  │   │ Provider│  │ Provider│  │  Peer   │  │  Peer   │ │             │
│  │   │  Node   │◄─┤  Node   │◄─┤  Node   │◄─┤  Node   │ │             │
│  │   └─────────┘  └─────────┘  └─────────┘  └─────────┘ │             │
│  └──────────────────────────────────────────────────────────┘             │
│                              │                                            │
│                              ▼                                            │
│  ┌──────────────────────────────────────────────────────────┐             │
│  │                    Web Interface                          │             │
│  │  ┌────────────┐  ┌────────────┐  ┌──────────────────┐  │             │
│  │  │ Model Store │  │   Search   │  │ Download Manager │  │             │
│  │  │     UI      │  │   Engine   │  │  (Iroh WASM)     │  │             │
│  │  └────────────┘  └────────────┘  └──────────────────┘  │             │
│  └──────────────────────────────────────────────────────────┘             │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Components

### 1. Model Catalog (`adnet-model-catalog`)

The core crate providing:

- **ModelManifest**: Metadata structure for AI models
- **ModelCatalog**: SQLite-based catalog for model metadata
- **ModelProvider**: Publishes models to the P2P network
- **ModelDownloader**: Downloads models via Iroh
- **ModelDiscovery**: Gossip-based model discovery

### 2. Model Manifest Schema

```rust
struct ModelManifest {
    id: String,              // UUID
    name: String,             // e.g., "Cyberpunk_LoRA"
    version: String,         // Semantic version
    model_type: ModelType,   // LLM, LoRA, VAE, Embedding, etc.
    size_bytes: u64,         // File size
    content_hash: String,    // BLAKE3 hash
    iroh_ticket: String,     // Iroh blob ticket
    author: String,           // Creator name
    description: String,     // Human-readable description
    tags: Vec<String>,       // Searchable tags
    architecture: String,     // e.g., "llama3", "stable-diffusion"
    quantization: Option<String>, // e.g., "Q4_K_M", "Q8_0"
    license: String,         // SPDX license
    created_at: DateTime,
    updated_at: DateTime,
}
```

### 3. P2P Distribution Flow

```
Provider Side:
1. Import model file to local blob store
2. Generate Iroh ticket for the blob
3. Create ModelManifest with ticket
4. Store manifest in local catalog
5. Announce model via Gossip Bus

Consumer Side:
1. Browse model catalog via Web UI
2. Search/filter models
3. Click "Download" to get ticket
4. Iroh downloads blob in background
5. Model available for local inference
```

### 4. Web Interface

The web interface provides:

- Model browsing with pagination
- Full-text search
- Category/tag filtering
- Model detail pages
- One-click download
- Download progress tracking

### 5. Iroh Integration

Using ADNet's existing Iroh integration:

- **Blob Transfer**: Bao-verified chunked download
- **NAT Traversal**: DERP relay for connectivity
- **Tickets**: Shareable download links
- **Gossip**: Model discovery announcements

## Usage Examples

### Adding a Model (Provider)

```rust
use adnet_model_catalog::{ModelCatalog, ModelProvider, ModelType};

let catalog = ModelCatalog::new("./catalog.db").await?;
let provider = ModelProvider::new(catalog.clone());

// Add a new model
provider.publish_model(
    path: "models/cyberpunk_lora.safetensors",
    name: "Cyberpunk_LoRA",
    model_type: ModelType::LoRA,
    author: "AI Artist",
    description: "Cyberpunk style LoRA for SDXL",
    tags: vec!["cyberpunk", "sci-fi", "sdxl"],
).await?;
```

### Browsing Models (Consumer)

```rust
use adnet_model_catalog::ModelCatalog;

let catalog = ModelCatalog::new("catalog.db").await?;

// List all models
let models = catalog.list_models().await?;

// Search by tag
let llms = catalog.search_by_tag("llm").await?;

// Get download ticket
let ticket = catalog.get_ticket("model-id").await?;
```

## Technical Decisions

1. **SQLite for Metadata**: Lightweight, zero-config, sufficient for catalog
2. **BLAKE3 for Content Hash**: Fast, secure, Bao-friendly
3. **Iroh Tickets**: Self-contained download credentials
4. **Gossip Discovery**: Epidemic broadcast for model announcements
5. **WASM Support**: Future browser-native downloads via Iroh WASM

## Future Enhancements

- [ ] Model rating and reviews
- [ ] Provider reputation system
- [ ] Incremental model updates (delta sync)
- [ ] WebAssembly browser client
- [ ] IPNS-style mutable naming for model updates
- [ ] Multi-provider redundancy ( erasure coding)
