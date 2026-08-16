//! A3Net-Eliza Bridge Integration Example
//!
//! Demonstrates how to:
//! 1. Create an Eliza agent identity on A3Net
//! 2. Connect the chat client to send/receive messages
//! 3. Connect the feed adapter to publish/subscribe news
//!
//! Run with: cargo run --example eliza_bridge

use a3net_eliza_bridge::{
    AdnetIdentity, ChatClientBuilder, ChatClientConfig,
    FeedAdapterBuilder,
};
use a3net_news::BulletinCategory;
use a3net_types::NodeId;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== A3Net-Eliza Bridge Demo ===\n");

    // Step 1: Create Agent Identity
    let data_dir = tempfile::tempdir()?;
    let agent_id = "deFi-analyst-001";

    let identity = AdnetIdentity::new(data_dir.path().to_path_buf(), agent_id).await?;
    println!("Step 1: Created identity");
    println!("  Agent ID: {}", agent_id);
    println!("  NodeId: {}", identity.node_id());
    println!("  Address: {}", identity.address().to_hex());

    // Step 2: Build Chat Client
    let chat_config = ChatClientConfig {
        display_name: "DeFi Analyst Bot".to_string(),
        agent_type: identity.profile().agent_type.clone(),
        ..Default::default()
    };
    let chat_client = ChatClientBuilder::new(identity.clone())
        .config(chat_config)
        .build()
        .await?;
    println!("\nStep 2: Chat client created");

    // Step 3: Build Feed Adapter
    let feed_adapter = FeedAdapterBuilder::new(identity.clone())
        .display_name("DeFi Analyst Bot")
        .categories(vec![BulletinCategory::Tech, BulletinCategory::Economy])
        .build()
        .await?;
    println!("Step 3: Feed adapter created");

    // Step 4: Connect
    chat_client.login().await?;
    feed_adapter.connect().await?;
    println!("Step 4: Connected to A3Net network");

    // Step 5: Subscribe
    feed_adapter.subscribe("defi-news").await?;
    feed_adapter.subscribe("market-alerts").await?;
    feed_adapter
        .subscribe_to_category(BulletinCategory::Economy)
        .await?;
    let subscriptions = feed_adapter.get_subscriptions().await;
    println!("Step 5: Subscribed to {} topics", subscriptions.len());

    // Step 6: Generate Eliza tools
    let chat_tools = chat_client.generate_eliza_tools();
    let feed_tools = feed_adapter.generate_eliza_tools();
    println!(
        "Step 6: Generated {} chat tools + {} feed tools",
        chat_tools.len(),
        feed_tools.len()
    );

    // Step 7: Send a message
    let recipient = NodeId::random();
    let mid = chat_client
        .send_message(&recipient, "Hello from DeFi Analyst!")
        .await?;
    println!("Step 7: Sent message id={}", mid);

    // Step 8: Publish a report
    let report_id = feed_adapter
        .publish_report(
            "Weekly DeFi Market Analysis",
            "TVL rose 12% this week. Lido and Aave led the gains.",
            BulletinCategory::Economy,
            vec!["defi".to_string(), "weekly".to_string()],
        )
        .await?;
    println!("Step 8: Published report id={}", report_id);

    // Step 9: Publish an alert
    let alert_id = feed_adapter
        .publish_alert(
            "BlackRock BUIDL Surpasses $500M",
            "BlackRock's tokenized US Treasury fund continues to grow.",
            a3net_news::BulletinSeverity::Notable,
            vec!["rwa".to_string()],
        )
        .await?;
    println!("Step 9: Published alert id={}", alert_id);

    // Step 10: Cleanup
    chat_client.logout().await?;
    feed_adapter.disconnect().await?;
    println!("\n=== Demo Complete ===");

    Ok(())
}
