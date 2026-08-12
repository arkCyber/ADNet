//! ADNet Model Catalog CLI
//!
//! Command-line interface for the AI Model Distribution Network

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tokio::fs;
use tracing::{info, error};

use adnet_model_catalog::{
    ModelCatalog, ModelProvider, ModelDownloader, ModelDiscovery,
    ModelType, Quantization, ModelFilter, SortField,
    provider::ModelMetadata,
    types::{DownloadStatus, CatalogStats},
    manifest::format_size,
};

#[derive(Parser)]
#[command(name = "adnet-model-catalog")]
#[command(about = "ADNet AI Model Distribution Network - CLI", long_about = None)]
struct Cli {
    /// Path to the model catalog database
    #[arg(long, default_value = "model-catalog.db")]
    catalog: PathBuf,

    /// Path for downloaded models
    #[arg(long, default_value = "./downloads")]
    download_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the model catalog web server
    Serve {
        /// Host to bind to
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        /// Port to bind to
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },

    /// List all models in the catalog
    List {
        /// Filter by model type
        #[arg(long)]
        model_type: Option<String>,

        /// Filter by architecture
        #[arg(long)]
        architecture: Option<String>,

        /// Filter by author
        #[arg(long)]
        author: Option<String>,

        /// Sort field
        #[arg(long, value_enum, default_value = "created")]
        sort: SortOption,

        /// Show only models matching search query
        #[arg(short, long)]
        search: Option<String>,

        /// Maximum number of results
        #[arg(long, default_value = "50")]
        limit: u64,

        /// Offset for pagination
        #[arg(long, default_value = "0")]
        offset: u64,

        /// Show statistics
        #[arg(short, long)]
        stats: bool,
    },

    /// Add a model to the catalog
    Add {
        /// Path to the model file
        #[arg(long)]
        path: PathBuf,

        /// Model name
        #[arg(long)]
        name: String,

        /// Model version
        #[arg(long, default_value = "1.0.0")]
        version: String,

        /// Model type
        #[arg(long, value_enum)]
        model_type: ModelTypeArg,

        /// Author name
        #[arg(long)]
        author: String,

        /// Description
        #[arg(long)]
        description: String,

        /// Architecture (e.g., llama3, sdxl)
        #[arg(long)]
        architecture: String,

        /// Quantization (e.g., Q4_K_M, Q8_0, none)
        #[arg(long, default_value = "none")]
        quantization: Option<String>,

        /// License (e.g., MIT, Apache-2.0)
        #[arg(long, default_value = "UNKNOWN")]
        license: String,

        /// Tags (comma-separated)
        #[arg(long)]
        tags: Option<String>,

        /// Source URL (e.g., HuggingFace model link)
        #[arg(long)]
        source_url: Option<String>,
    },

    /// Search for models
    Search {
        /// Search query
        query: String,

        /// Maximum results
        #[arg(long, default_value = "20")]
        limit: u64,
    },

    /// Get detailed info about a model
    Info {
        /// Model ID or name
        model_id: String,
    },

    /// Download a model
    Download {
        /// Model ID to download
        model_id: String,

        /// Output directory
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Update a model's metadata
    Update {
        /// Model ID to update
        model_id: String,

        /// New name
        #[arg(long)]
        name: Option<String>,

        /// New description
        #[arg(long)]
        description: Option<String>,

        /// New tags (comma-separated, replaces existing)
        #[arg(long)]
        tags: Option<String>,

        /// New version
        #[arg(long)]
        version: Option<String>,

        /// New license
        #[arg(long)]
        license: Option<String>,

        /// New source URL
        #[arg(long)]
        source_url: Option<String>,
    },

    /// Remove a model from the catalog (soft delete)
    Remove {
        /// Model ID to remove
        model_id: String,

        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Permanently delete a model from the catalog AND the blob store (hard delete)
    Delete {
        /// Model ID to delete
        model_id: String,

        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Show catalog statistics
    Stats,

    /// List all available tags
    Tags,

    /// List all architectures
    Architectures,

    /// List providers with their reputation scores
    Providers {
        /// Show only blocked providers
        #[arg(long)]
        blocked: bool,

        /// Show only trusted providers
        #[arg(long)]
        trusted: bool,
    },

    /// Show reputation info for a specific provider
    ProviderInfo {
        /// Provider node ID (64 hex chars)
        node_id: String,
    },

    /// Mark a provider as trusted / blocked / neutral
    ProviderFlag {
        /// Provider node ID (64 hex chars)
        node_id: String,

        /// New trust flag: trusted, blocked, neutral
        flag: String,
    },

    /// Submit a user report against a provider
    ReportProvider {
        /// Provider node ID (64 hex chars)
        node_id: String,

        /// Reason: spam, harassment, impersonation, phishing,
        /// misleading_metadata, license_violation, malicious_content, other
        reason: String,

        /// Free-form detail / evidence
        #[arg(long)]
        detail: Option<String>,
    },

    /// Import models from a directory
    Import {
        /// Directory containing model files
        path: PathBuf,

        /// Model type for all files
        #[arg(long, value_enum)]
        model_type: ModelTypeArg,

        /// Author name
        #[arg(long)]
        author: String,

        /// Recursively scan subdirectories
        #[arg(short, long)]
        recursive: bool,
    },
}

#[derive(ValueEnum, Clone)]
enum SortOption {
    Name,
    Created,
    Updated,
    Size,
    Downloads,
}

impl From<SortOption> for SortField {
    fn from(s: SortOption) -> Self {
        match s {
            SortOption::Name => SortField::Name,
            SortOption::Created => SortField::CreatedAt,
            SortOption::Updated => SortField::UpdatedAt,
            SortOption::Size => SortField::Size,
            SortOption::Downloads => SortField::Downloads,
        }
    }
}

#[derive(ValueEnum, Clone)]
enum ModelTypeArg {
    Llm,
    Lora,
    TextToImage,
    ImageToImage,
    ControlNet,
    Vae,
    TextToVideo,
    ImageToVideo,
    Embedding,
    SpeechToText,
    TextToSpeech,
    Vision,
    Multilingual,
}

impl From<ModelTypeArg> for ModelType {
    fn from(t: ModelTypeArg) -> Self {
        match t {
            ModelTypeArg::Llm => ModelType::Llm,
            ModelTypeArg::Lora => ModelType::Lora,
            ModelTypeArg::TextToImage => ModelType::TextToImage,
            ModelTypeArg::ImageToImage => ModelType::ImageToImage,
            ModelTypeArg::ControlNet => ModelType::ControlNet,
            ModelTypeArg::Vae => ModelType::Vae,
            ModelTypeArg::TextToVideo => ModelType::TextToVideo,
            ModelTypeArg::ImageToVideo => ModelType::ImageToVideo,
            ModelTypeArg::Embedding => ModelType::Embedding,
            ModelTypeArg::SpeechToText => ModelType::SpeechToText,
            ModelTypeArg::TextToSpeech => ModelType::TextToSpeech,
            ModelTypeArg::Vision => ModelType::Vision,
            ModelTypeArg::Multilingual => ModelType::Multilingual,
        }
    }
}

fn parse_quantization(s: &str) -> Quantization {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("full") {
        Quantization::None
    } else if s.starts_with("Q4") || s.starts_with("q4") {
        Quantization::Q4(s.replace("Q4", "").replace("q4", "").trim_start_matches('_').to_string())
    } else if s.starts_with("Q8") || s.starts_with("q8") {
        Quantization::Q8(s.replace("Q8", "").replace("q8", "").trim_start_matches('_').to_string())
    } else if s.starts_with("GPTQ") || s.starts_with("gptq") {
        Quantization::GPTQ(s.replace("GPTQ", "").trim_start_matches(' ').to_string())
    } else if s.starts_with("AWQ") || s.starts_with("awq") {
        Quantization::AWQ(s.replace("AWQ", "").trim_start_matches(' ').to_string())
    } else if s.starts_with("GGUF") || s.starts_with("gguf") {
        Quantization::GGUF(s.replace("GGUF", "").trim_start_matches(' ').to_string())
    } else if s == "fp16" || s == "FP16" || s == "bf16" || s == "BF16" {
        Quantization::FP16
    } else {
        Quantization::Other(s.to_string())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Ensure download directory exists
    fs::create_dir_all(&cli.download_dir).await?;

    // Initialize catalog
    let catalog = ModelCatalog::open(&cli.catalog).await?;
    let catalog = Arc::new(catalog);

    match cli.command {
        Commands::Serve { host, port } => {
            info!("Starting server on {}:{}", host, port);
            
            let config = adnet_model_catalog::server::ServerConfig::new(&host, port)
                .with_catalog_path(&cli.catalog)
                .with_download_dir(&cli.download_dir);

            adnet_model_catalog::server::start(config).await?;
        }

        Commands::List { model_type, architecture, author, sort, search, limit, offset, stats } => {
            let filter = ModelFilter {
                model_type: model_type.map(|t| ModelType::parse(&t)),
                architecture,
                author,
                query: search,
                sort_by: Some(sort.into()),
                limit: Some(limit),
                offset: Some(offset),
                ..Default::default()
            };

            let results = catalog.list(filter).await?;

            if stats {
                let stats = catalog.stats().await?;
                print_stats(&stats);
                println!();
            }

            println!("Found {} models:\n", results.total);
            
            for model in &results.items {
                println!("┌────────────────────────────────────────────────────────────────");
                println!("│ {} ({})", model.name, model.id);
                println!("│ Type: {} | Architecture: {} | Quantization: {}", 
                    model.model_type, model.architecture, model.quantization.display_name());
                println!("│ Size: {} | Downloads: {} | License: {}", 
                    model.size_display(), model.download_count, model.license);
                println!("│ Author: {} | Added: {}", 
                    model.author, model.created_at.format("%Y-%m-%d"));
                if !model.tags.is_empty() {
                    println!("│ Tags: {}", model.tags.join(", "));
                }
                println!("│ Hash: {}", &model.content_hash[..16]);
                println!("│ Ticket: {}", &model.iroh_ticket[..60]);
                println!("└────────────────────────────────────────────────────────────────\n");
            }

            if results.has_more() {
                println!("Showing {} of {} models. Use --offset {} --limit {} to see more.",
                    results.items.len(), results.total, offset + limit, limit);
            }
        }

        Commands::Add { path, name, version, model_type, author, description, architecture, quantization, license, tags, source_url } => {
            let tags: Vec<String> = tags
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default();

            let quantization = parse_quantization(quantization.as_deref().unwrap_or("none"));

            let provider = ModelProvider::new(catalog.clone());

            let metadata = ModelMetadata {
                name,
                version,
                model_type: model_type.into(),
                author,
                description,
                tags,
                architecture,
                quantization,
                license,
                source_url: source_url.map(String::from),
            };

            let manifest = provider
                .publish_model(&path, metadata)
                .await?;

            println!("✓ Model added successfully!");
            println!("  ID: {}", manifest.id);
            println!("  Hash: {}", manifest.content_hash);
            println!("  Ticket: {}", manifest.iroh_ticket);
        }

        Commands::Search { query, limit } => {
            let results = catalog.search(&query).await?;

            println!("Found {} results for '{}':\n", results.len(), query);

            for (i, model) in results.iter().take(limit as usize).enumerate() {
                println!("{}. {} ({})", i + 1, model.name, model.model_type);
                println!("   {} | {} | by {}", model.size_display(), model.architecture, model.author);
                println!("   ID: {}", model.id);
                println!();
            }
        }

        Commands::Info { model_id } => {
            // Try to find by ID first, then by name
            let manifest = if let Some(m) = catalog.get(&model_id).await? {
                m
            } else {
                // Search by name
                let results = catalog.search(&model_id).await?;
                if results.is_empty() {
                    error!("Model not found: {}", model_id);
                    return Ok(());
                }
                results.into_iter().next().unwrap()
            };

            println!("{}", "═".repeat(60));
            println!("  {}", manifest.name);
            println!("{}", "═".repeat(60));
            println!();
            println!("  ID:           {}", manifest.id);
            println!("  Version:      {}", manifest.version);
            println!("  Type:         {}", manifest.model_type);
            println!("  Architecture: {}", manifest.architecture);
            println!("  Quantization: {}", manifest.quantization.display_name());
            println!();
            println!("  Size:         {}", manifest.size_display());
            println!("  License:      {}", manifest.license);
            println!("  Author:       {}", manifest.author);
            println!();
            println!("  Downloads:    {}", manifest.download_count);
            println!("  Created:      {}", manifest.created_at.format("%Y-%m-%d %H:%M UTC"));
            println!("  Updated:      {}", manifest.updated_at.format("%Y-%m-%d %H:%M UTC"));
            println!();
            println!("  Description:");
            for line in textwrap::wrap(&manifest.description, 56) {
                println!("    {}", line);
            }
            println!();
            println!("  Tags: {}", if manifest.tags.is_empty() { "none".to_string() } else { manifest.tags.join(", ") });
            println!();
            println!("  Content Hash (BLAKE3):");
            println!("    {}", manifest.content_hash);
            println!();
            println!("  Iroh Ticket:");
            println!("    {}", manifest.iroh_ticket);
            println!("{}", "═".repeat(60));
        }

        Commands::Download { model_id, output } => {
            let output_dir = output.unwrap_or(cli.download_dir.clone());
            let downloader = ModelDownloader::new(catalog.clone(), output_dir);

            let ticket = catalog.get_ticket(&model_id).await?
                .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

            println!("Starting download for model: {}", model_id);
            println!("Using ticket: {}...", &ticket[..40]);

            let handle = downloader.download(model_id.clone(), ticket).await?;

            // Wait for download to complete
            let result = handle.await_completion().await?;

            match result.status {
                DownloadStatus::Completed => {
                    println!("✓ Download completed: {} bytes", result.bytes_downloaded);
                }
                DownloadStatus::Failed(e) => {
                    error!("Download failed: {}", e);
                }
                _ => {
                    println!("Download status: {:?}", result.status);
                }
            }
        }

        Commands::Update { model_id, name, description, tags, version, license, source_url } => {
            let provider = ModelProvider::new(catalog.clone());

            let tags: Option<Vec<String>> = tags.map(|s| {
                s.split(',').map(|t| t.trim().to_string()).collect()
            });

            let updated = provider
                .update_metadata(&model_id, |m| {
                    if let Some(n) = name {
                        m.name = n;
                    }
                    if let Some(d) = description {
                        m.description = d;
                    }
                    if let Some(t) = tags {
                        m.tags = t;
                    }
                    if let Some(v) = version {
                        m.version = v;
                    }
                    if let Some(l) = license {
                        m.license = l;
                    }
                    if let Some(u) = source_url {
                        m.source_url = Some(u);
                    }
                })
                .await?;

            println!("✓ Model updated: {}", updated.name);
            println!("  ID: {}", updated.id);
            if let Some(ref url) = updated.source_url {
                println!("  Source: {}", url);
            }
        }

        Commands::Remove { model_id, force } => {
            let manifest = catalog.get(&model_id).await?
                .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

            if !force {
                println!("Are you sure you want to remove '{}'? [y/N]", manifest.name);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                let confirmed = input.trim().eq_ignore_ascii_case("y")
                    || input.trim().eq_ignore_ascii_case("yes");
                if !confirmed {
                    println!("Aborted.");
                    return Ok(());
                }
            }

            catalog.remove(&model_id).await?;
            println!("✓ Model removed: {}", manifest.name);
        }

        Commands::Delete { model_id, force } => {
            let manifest = catalog.get(&model_id).await?
                .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

            if !force {
                println!("⚠ This will PERMANENTLY delete '{}' from the catalog AND blob store.", manifest.name);
                println!("   Content hash: {}", manifest.content_hash);
                println!("   Type: {} | Size: {}", manifest.model_type, manifest.size_display());
                println!("\nType 'yes' to confirm deletion: ");
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                if !input.trim().eq_ignore_ascii_case("yes") {
                    println!("Aborted.");
                    return Ok(());
                }
            }

            let provider = ModelProvider::new(catalog.clone());
            provider.delete_model(&model_id).await?;
            println!("✓ Model permanently deleted: {}", manifest.name);
        }

        Commands::Stats => {
            let stats = catalog.stats().await?;
            print_stats(&stats);
        }

        Commands::Tags => {
            let tags = catalog.get_all_tags().await?;
            
            println!("Available tags ({} total):\n", tags.len());
            
            let mut current_letter = ' ';
            for (tag, count) in tags {
                let first = tag.chars().next().unwrap_or(' ');
                if first != current_letter {
                    if current_letter != ' ' {
                        println!();
                    }
                    current_letter = first;
                    println!("─── {} ───", current_letter.to_uppercase());
                }
                println!("  {} ({})", tag, count);
            }
        }

        Commands::Architectures => {
            let architectures = catalog.get_all_architectures().await?;

            println!("Available architectures ({} total):\n", architectures.len());

            for arch in architectures {
                println!("  • {}", arch);
            }
        }

        Commands::Providers { blocked, trusted } => {
            let reps = catalog.list_provider_reputation().await?;
            let filtered: Vec<_> = reps
                .into_iter()
                .filter(|r| {
                    if blocked {
                        matches!(
                            r.trust_flag,
                            adnet_model_catalog::TrustFlag::Blocked
                        ) || r.trust_tier() == adnet_model_catalog::TrustTier::Blocked
                    } else if trusted {
                        matches!(
                            r.trust_flag,
                            adnet_model_catalog::TrustFlag::Trusted
                        ) || r.trust_tier() == adnet_model_catalog::TrustTier::Trusted
                    } else {
                        true
                    }
                })
                .collect();

            if filtered.is_empty() {
                println!("No providers tracked.");
                return Ok(());
            }

            println!(
                "{:<14}  {:<8}  {:<14}  {:>7}  {:>7}  {:>7}  {:<19}",
                "NODE", "TIER", "FLAG", "SCORE", "OK", "FAIL", "LAST"
            );
            println!("{}", "─".repeat(86));
            for r in filtered {
                println!(
                    "{:<14}  {:<8}  {:<14}  {:>7.2}  {:>7}  {:>7}  {:<19}",
                    &r.node_id[..12.min(r.node_id.len())],
                    format!("{:?}", r.trust_tier()),
                    format!("{:?}", r.trust_flag),
                    r.score,
                    r.successful_downloads,
                    r.failed_downloads,
                    r.last_updated.to_rfc3339(),
                );
            }
        }

        Commands::ProviderInfo { node_id } => {
            let rep = catalog
                .get_provider_reputation(&node_id)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!("No reputation recorded for {}", node_id)
                })?;
            println!("Provider: {}", rep.node_id);
            println!("  Score:           {:.2}", rep.score);
            println!("  Trust tier:      {:?}", rep.trust_tier());
            println!("  Manual flag:     {:?}", rep.trust_flag);
            println!("  Success rate:    {:?}",
                rep.success_rate().map(|r| format!("{:.1}%", r * 100.0))
                    .unwrap_or_else(|| "n/a".to_string()));
            println!("  Successful DLs:  {}", rep.successful_downloads);
            println!("  Failed DLs:      {}", rep.failed_downloads);
            println!("  Reports:         {}", rep.reports_count);
            println!("  Last updated:    {}", rep.last_updated.to_rfc3339());
        }

        Commands::ProviderFlag { node_id, flag } => {
            let parsed = match flag.as_str() {
                "trusted" => adnet_model_catalog::TrustFlag::Trusted,
                "blocked" => adnet_model_catalog::TrustFlag::Blocked,
                "neutral" => adnet_model_catalog::TrustFlag::Neutral,
                other => anyhow::bail!(
                    "unknown flag '{}': expected trusted|blocked|neutral",
                    other
                ),
            };
            let mut rep = catalog
                .get_provider_reputation(&node_id)
                .await?
                .unwrap_or_else(|| {
                    adnet_model_catalog::ProviderReputation {
                        node_id: node_id.clone(),
                        score: 0.0,
                        successful_downloads: 0,
                        failed_downloads: 0,
                        reports_count: 0,
                        last_updated: chrono::Utc::now(),
                        trust_flag: adnet_model_catalog::TrustFlag::Neutral,
                    }
                });
            rep.trust_flag = parsed;
            rep.last_updated = chrono::Utc::now();
            catalog.upsert_provider_reputation(&rep).await?;
            println!(
                "✓ Provider {} marked as {:?} (tier: {:?})",
                node_id,
                parsed,
                rep.trust_tier()
            );
        }

        Commands::ReportProvider { node_id, reason, detail } => {
            let parsed = match reason.as_str() {
                "spam" => adnet_model_catalog::ReportReason::Spam,
                "harassment" => adnet_model_catalog::ReportReason::Harassment,
                "impersonation" => adnet_model_catalog::ReportReason::Impersonation,
                "phishing" => adnet_model_catalog::ReportReason::Phishing,
                "misleading_metadata" => {
                    adnet_model_catalog::ReportReason::MisleadingMetadata
                }
                "license_violation" => {
                    adnet_model_catalog::ReportReason::LicenseViolation
                }
                "malicious_content" => {
                    adnet_model_catalog::ReportReason::MaliciousContent
                }
                "other" => adnet_model_catalog::ReportReason::Other,
                other => anyhow::bail!(
                    "unknown reason '{}': expected spam|harassment|impersonation|phishing|\
                     misleading_metadata|license_violation|malicious_content|other",
                    other
                ),
            };
            let tracker = adnet_model_catalog::ProviderReputationTracker::new();
            tracker.report_provider(&node_id, parsed, 0, detail.clone())?;
            // Persist the snapshot back to disk so the report is
            // visible from the CLI on next read.
            let snap = tracker.get(&node_id).unwrap();
            catalog.upsert_provider_reputation(&snap).await?;
            println!(
                "✓ Filed {:?} report against provider {} (total reports: {})",
                parsed,
                &node_id[..12.min(node_id.len())],
                snap.reports_count
            );
        }

        Commands::Import { path, model_type, author, recursive } => {
            let provider = ModelProvider::new(catalog.clone());

            let model_type: ModelType = model_type.into();

            let entries = if recursive {
                walkdir(&path).await?
            } else {
                let mut entries = Vec::new();
                let mut dir = fs::read_dir(&path).await?;
                while let Some(entry) = dir.next_entry().await? {
                    if entry.file_type().await?.is_file() {
                        entries.push(entry.path());
                    }
                }
                entries
            };

            println!("Found {} model files to import\n", entries.len());

            for (i, file_path) in entries.iter().enumerate() {
                let name = file_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("Model_{}", i));

                print!("[{}/{}] Importing {}... ", i + 1, entries.len(), name);

                match provider.publish_model(file_path, ModelMetadata {
                    name: name.clone(),
                    version: "1.0.0".to_string(),
                    model_type: model_type.clone(),
                    author: author.clone(),
                    description: format!("Auto-imported {} model", model_type),
                    tags: vec!["imported".to_string()],
                    architecture: "unknown".to_string(),
                    quantization: Quantization::None,
                    license: "UNKNOWN".to_string(),
                    source_url: None,
                }).await {
                    Ok(manifest) => {
                        println!("✓ ({})", manifest.id);
                    }
                    Err(e) => {
                        println!("✗ Error: {}", e);
                    }
                }
            }

            println!("\nImport complete!");
        }
    }

    Ok(())
}

async fn walkdir(path: &PathBuf) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![path.clone()];

    while let Some(current) = stack.pop() {
        let mut dir = fs::read_dir(&current).await?;
        while let Some(entry) = dir.next_entry().await? {
            let entry_path = entry.path();
            if entry.file_type().await?.is_dir() {
                stack.push(entry_path);
            } else {
                files.push(entry_path);
            }
        }
    }

    Ok(files)
}

fn print_stats(stats: &CatalogStats) {
    println!("{}", "═".repeat(50));
    println!("  ADNet Model Catalog Statistics");
    println!("{}", "═".repeat(50));
    println!();
    println!("  Total Models:    {}", stats.total_models);
    println!("  Total Size:      {}", format_size(stats.total_size_bytes));
    println!("  Recent (7 days): {}", stats.recent_models);
    println!();
    println!("  Models by Type:");
    for (model_type, count) in &stats.models_by_type {
        println!("    {:16} {}", format!("{}:", model_type), count);
    }
    println!("{}", "═".repeat(50));
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_model_catalog::Quantization;
    use clap::Parser;

    // ── parse_quantization ───────────────────────────────────────

    #[test]
    fn parse_quantization_none_variants() {
        assert_eq!(parse_quantization("none"), Quantization::None);
        assert_eq!(parse_quantization("NONE"), Quantization::None);
        assert_eq!(parse_quantization("None"), Quantization::None);
        assert_eq!(parse_quantization("full"), Quantization::None);
        assert_eq!(parse_quantization("FULL"), Quantization::None);
        assert_eq!(parse_quantization(""), Quantization::None);
        assert_eq!(parse_quantization("  "), Quantization::None);
    }

    #[test]
    fn parse_quantization_q4() {
        assert_eq!(
            parse_quantization("Q4_K_M"),
            Quantization::Q4("K_M".to_string())
        );
        assert_eq!(parse_quantization("Q4"), Quantization::Q4("".to_string()));
        assert_eq!(
            parse_quantization("q4_k_m"),
            Quantization::Q4("k_m".to_string())
        );
    }

    #[test]
    fn parse_quantization_q8() {
        assert_eq!(
            parse_quantization("Q8_0"),
            Quantization::Q8("0".to_string())
        );
        assert_eq!(
            parse_quantization("q8_0"),
            Quantization::Q8("0".to_string())
        );
    }

    #[test]
    fn parse_quantization_gptq() {
        assert_eq!(
            parse_quantization("GPTQ-4bit"),
            Quantization::GPTQ("-4bit".to_string())
        );
    }

    #[test]
    fn parse_quantization_awq() {
        assert_eq!(
            parse_quantization("AWQ-4bit"),
            Quantization::AWQ("-4bit".to_string())
        );
    }

    #[test]
    fn parse_quantization_gguf() {
        assert_eq!(
            parse_quantization("GGUF-Q8"),
            Quantization::GGUF("-Q8".to_string())
        );
    }

    #[test]
    fn parse_quantization_fp16_bf16() {
        assert_eq!(parse_quantization("fp16"), Quantization::FP16);
        assert_eq!(parse_quantization("FP16"), Quantization::FP16);
        assert_eq!(parse_quantization("bf16"), Quantization::FP16);
        assert_eq!(parse_quantization("BF16"), Quantization::FP16);
    }

    #[test]
    fn parse_quantization_unknown_falls_through_to_other() {
        assert_eq!(
            parse_quantization("mxfp4"),
            Quantization::Other("mxfp4".to_string())
        );
    }

    #[test]
    fn parse_quantization_strips_whitespace() {
        assert_eq!(parse_quantization("  Q4_K_M  "), Quantization::Q4("K_M".to_string()));
    }

    // ── CLI parser smoke tests ──────────────────────────────────

    #[test]
    fn cli_parses_serve_command() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "--catalog", "/tmp/c.db",
            "--download-dir", "/tmp/dl",
            "serve",
            "--host", "127.0.0.1",
            "--port", "9999",
        ])
        .expect("parse");
        match cli.command {
            Commands::Serve { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 9999);
            }
            _ => panic!("expected Serve"),
        }
    }

    #[test]
    fn cli_parses_list_command() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "list",
            "--limit", "10",
            "--offset", "5",
        ])
        .expect("parse");
        match cli.command {
            Commands::List { limit, offset, .. } => {
                assert_eq!(limit, 10);
                assert_eq!(offset, 5);
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn cli_parses_providers_command_with_filters() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "providers",
            "--trusted",
        ])
        .expect("parse");
        match cli.command {
            Commands::Providers { trusted, blocked } => {
                assert!(trusted);
                assert!(!blocked);
            }
            _ => panic!("expected Providers"),
        }
    }

    #[test]
    fn cli_parses_provider_flag_command() {
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "provider-flag",
            node,
            "blocked",
        ])
        .expect("parse");
        match cli.command {
            Commands::ProviderFlag { node_id, flag } => {
                assert_eq!(node_id, node);
                assert_eq!(flag, "blocked");
            }
            _ => panic!("expected ProviderFlag"),
        }
    }

    #[test]
    fn cli_parses_report_provider_command() {
        let node = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "report-provider",
            node,
            "spam",
            "--detail", "duplicate upload",
        ])
        .expect("parse");
        match cli.command {
            Commands::ReportProvider { node_id, reason, detail } => {
                assert_eq!(node_id, node);
                assert_eq!(reason, "spam");
                assert_eq!(detail.as_deref(), Some("duplicate upload"));
            }
            _ => panic!("expected ReportProvider"),
        }
    }

    #[test]
    fn cli_parses_search_command() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "search",
            "llama3",
            "--limit", "5",
        ])
        .expect("parse");
        match cli.command {
            Commands::Search { query, limit } => {
                assert_eq!(query, "llama3");
                assert_eq!(limit, 5);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn cli_parses_update_command() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "update",
            "abc123",
            "--description", "New description",
            "--tags", "chat,instruct",
        ])
        .expect("parse");
        match cli.command {
            Commands::Update { model_id, description, tags, .. } => {
                assert_eq!(model_id, "abc123");
                assert_eq!(description.as_deref(), Some("New description"));
                assert_eq!(tags.as_deref(), Some("chat,instruct"));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        let r = Cli::try_parse_from(["adnet-model-catalog", "bogus"]);
        assert!(r.is_err());
    }

    #[test]
    fn cli_uses_default_catalog_path() {
        let cli = Cli::try_parse_from(["adnet-model-catalog", "stats"]).expect("parse");
        assert_eq!(cli.catalog, PathBuf::from("model-catalog.db"));
        assert_eq!(cli.download_dir, PathBuf::from("./downloads"));
    }

    #[test]
    fn cli_parses_add_command_with_source_url() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "add",
            "--path", "/tmp/model.bin",
            "--name", "llama3-8b",
            "--model-type", "llm",
            "--author", "Meta",
            "--description", "Instruct model",
            "--architecture", "llama3",
            "--quantization", "Q4_K_M",
            "--source-url", "https://huggingface.co/meta/llama3-8b",
        ])
        .expect("parse");
        match cli.command {
            Commands::Add { name, source_url, quantization, .. } => {
                assert_eq!(name, "llama3-8b");
                assert_eq!(source_url.as_deref(), Some("https://huggingface.co/meta/llama3-8b"));
                assert_eq!(quantization.as_deref(), Some("Q4_K_M"));
            }
            _ => panic!("expected Add"),
        }
    }

    #[test]
    fn cli_add_default_quantization_is_none() {
        let cli = Cli::try_parse_from([
            "adnet-model-catalog",
            "add",
            "--path", "/tmp/model.bin",
            "--name", "x",
            "--model-type", "llm",
            "--author", "a",
            "--description", "d",
            "--architecture", "arch",
        ])
        .expect("parse");
        match cli.command {
            Commands::Add { quantization, .. } => {
                assert_eq!(quantization.as_deref(), Some("none"));
            }
            _ => panic!("expected Add"),
        }
    }
}
