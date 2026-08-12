# ADNet AI Model Distribution Network

A decentralized P2P system for distributing AI models built on top of ADNet's Iroh-based infrastructure.

## Overview

The ADNet Model Catalog enables:
- **Decentralized Distribution**: Share AI models via P2P network without central servers
- **Bao-Verified Transfers**: Cryptographically verified content integrity
- **NAT Traversal**: Works behind firewalls via DERP relay servers
- **Model Discovery**: Gossip-based model announcements
- **Beautiful Web UI**: Modern model store interface

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
│  │  └────────────┘  └────────────┘  └──────────────────────┘ │             │
│  └──────────────────────────────────────────────────────────┘             │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Quick Start

### 1. Add a Model to Your Catalog

```bash
# Build the CLI
cargo build -p adnet-model-catalog --features server

# Add a model
./target/debug/adnet-model-catalog add \
    --path ./models/my_lora.safetensors \
    --name "Cyberpunk_LoRA" \
    --model-type lora \
    --author "Your Name" \
    --description "Cyberpunk style LoRA for Stable Diffusion" \
    --architecture stable-diffusion \
    --tags "cyberpunk,sci-fi,art"
```

### 2. Start the Web Server

```bash
# Start the catalog server
./target/debug/adnet-model-catalog serve --host 0.0.0.0 --port 8080

# Or use the library directly
cargo run -p adnet-model-catalog --features server -- serve
```

### 3. Browse Models

Open http://localhost:8080 in your browser to see the model store UI.

## Usage

### Using as a Library

```rust
use adnet_model_catalog::{ModelCatalog, ModelProvider, ModelType, Quantization};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Open catalog
    let catalog = ModelCatalog::open("models.db").await?;
    let catalog = Arc::new(catalog);

    // Create provider
    let provider = ModelProvider::new(catalog.clone());

    // Publish a model
    let manifest = provider.publish_model(
        "path/to/model.bin",
        "MyModel".to_string(),
        "1.0.0".to_string(),
        ModelType::Llm,
        "Author".to_string(),
        "Description".to_string(),
        vec!["tag1".to_string()],
        "llama3".to_string(),
        Quantization::Q4("K_M".to_string()),
        "MIT".to_string(),
    ).await?;

    println!("Published: {}", manifest.id);

    // List models
    let models = catalog.list(Default::default()).await?;
    println!("Total models: {}", models.total);

    Ok(())
}
```

### Using the CLI

```bash
# List all models
adnet-model-catalog list

# Search for models
adnet-model-catalog search "cyberpunk"

# Get model info
adnet-model-catalog info <model-id>

# Download a model
adnet-model-catalog download <model-id> --output ./models/

# Show statistics
adnet-model-catalog stats

# List all tags
adnet-model-catalog tags

# Import models from a directory
adnet-model-catalog import \
    --path ./models/ \
    --model-type lora \
    --author "My Name" \
    --recursive
```

## Model Types

The catalog supports various AI model types:

| Type | Description | Examples |
|------|-------------|----------|
| `llm` | Large Language Models | Llama, Mistral, GPT |
| `lora` | LoRA Adapters | Style LoRAs, Control LoRAs |
| `text_to_image` | Text-to-Image | Stable Diffusion, SDXL |
| `image_to_image` | Image-to-Image | Img2Img models |
| `controlnet` | ControlNet models | Canny, Depth |
| `vae` | VAE models | SD VAE, Anime VAE |
| `embedding` | Embedding models | BGE, E5 |
| `speech_to_text` | Speech Recognition | Whisper |
| `text_to_speech` | Speech Synthesis | XTTS |
| `vision` | Vision models | CLIP, SAM |

## Quantization Formats

| Format | Description |
|--------|-------------|
| `none` | Full precision (FP32) |
| `fp16` | Half precision (FP16/BF16) |
| `Q4_K_M` | 4-bit quantization, balanced |
| `Q8_0` | 8-bit quantization, high quality |
| `GGUF` | GGUF format (Q4_K_M implied) |
| `GPTQ` | GPTQ quantized |
| `AWQ` | AWQ quantized |

## API Reference

### REST API

The web server exposes a REST API at `/api/`:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/models` | GET | List models with filters |
| `/api/models/:id` | GET | Get model details |
| `/api/models/:id/ticket` | GET | Get Iroh download ticket |
| `/api/stats` | GET | Get catalog statistics |
| `/api/tags` | GET | List all tags |
| `/api/search?q=` | GET | Search models |

### Example API Usage

```bash
# Get all LLMs
curl http://localhost:8080/api/models?type=llm

# Search for models
curl http://localhost:8080/api/search?q=cyberpunk

# Get download ticket
curl http://localhost:8080/api/models/<id>/ticket

# Get statistics
curl http://localhost:8080/api/stats
```

## Model Manifest Schema

```json
{
  "id": "uuid-v4",
  "name": "Model Name",
  "version": "1.0.0",
  "model_type": "llm",
  "size_bytes": 4294967296,
  "content_hash": "blake3-hash-64-chars",
  "iroh_ticket": "iroh://blob/...",
  "author": "Author Name",
  "description": "Model description",
  "tags": ["tag1", "tag2"],
  "architecture": "llama3",
  "quantization": "Q4_K_M",
  "license": "MIT",
  "download_count": 1234,
  "created_at": "2024-01-01T00:00:00Z",
  "updated_at": "2024-01-01T00:00:00Z"
}
```

## Features

### 1. Full-Text Search

The catalog uses SQLite FTS5 for fast full-text search across model names, descriptions, and tags.

### 2. Tag-Based Filtering

Models can be tagged with arbitrary labels for easy categorization and filtering.

### 3. Download Tracking

Each model tracks download counts and provides download statistics.

### 4. Gossip-Based Discovery

When running with Iroh support, models are announced via gossip protocol for peer discovery.

### 5. Bao Verification

All model transfers are verified using Bao (BLAKE3 authenticated organization) for cryptographic integrity.

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ADNET_CATALOG_DB` | `model-catalog.db` | Path to catalog database |
| `ADNET_DOWNLOAD_DIR` | `./downloads` | Directory for downloaded models |
| `ADNET_SERVER_HOST` | `0.0.0.0` | Server bind address |
| `ADNET_SERVER_PORT` | `8080` | Server port |

### Feature Flags

| Feature | Description |
|---------|-------------|
| `iroh` | Enable Iroh P2P integration (blob transfer, gossip) |
| `server` | Enable web server and API |

## Future Enhancements

- [ ] Model rating and reviews system
- [ ] Provider reputation/trust system
- [ ] Incremental model updates (delta sync)
- [ ] WebAssembly browser client for direct downloads
- [ ] IPNS-style mutable naming for model updates
- [ ] Multi-provider redundancy using erasure coding
- [ ] Model recommendation engine
- [ ] Automatic quantization detection
- [ ] Integration with HuggingFace Hub

## License

MIT OR Apache-2.0
