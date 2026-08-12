//! Example: CLI parsing utilities
//!
//! Demonstrates how to:
//! - Parse command-line arguments programmatically
//! - Work with subcommands and nested args
//! - Validate and process CLI input
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-cli --example cli_parser
//! ```

use adnet_cli::{Cli, Cmd, ConfigCmd, IrohOpt};
use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    println!("=== ADNet CLI Parser Demo ===\n");

    // 1. Basic init command
    println!("1. Parsing 'adnet init':");
    let cli = Cli::try_parse_from(["adnet", "init"])?;
    assert!(matches!(cli.cmd, Cmd::Init));
    println!("   ✓ Init command parsed correctly");
    println!("   Data dir: {}", cli.data_dir);

    // 2. Echo with room
    println!("\n2. Parsing 'adnet echo --room lobby':");
    let cli = Cli::try_parse_from(["adnet", "echo", "--room", "my-room"])?;
    if let Cmd::Echo { room } = &cli.cmd {
        println!("   ✓ Echo command parsed");
        println!("   Room: {room}");
    }

    // 3. Announce with all options
    println!("\n3. Parsing 'adnet announce' with all options:");
    let cli = Cli::try_parse_from([
        "adnet",
        "--config",
        "/custom/config.json",
        "announce",
        "--room",
        "research",
        "--file",
        "/path/to/model.bin",
        "--title",
        "LLM v2.0",
        "--kind",
        "ai_model",
    ])?;
    if let Cmd::Announce {
        room,
        file,
        title,
        kind,
    } = &cli.cmd
    {
        println!("   ✓ Announce command parsed:");
        println!("   Room:  {room}");
        println!("   File:  {file}");
        println!("   Title: {title}");
        println!("   Kind:  {kind}");
    }
    println!("   Config: {}", cli.config.unwrap_or_default());

    // 4. Config subcommands
    println!("\n4. Config subcommands:");
    
    // ConfigCmd::Path
    let cli = Cli::try_parse_from(["adnet", "config", "path"])?;
    let _desc = match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Path => { println!("   ✓ ConfigCmd::Path -> Path"); }
            _ => {}
        },
        _ => {}
    };

    // ConfigCmd::Show
    let cli = Cli::try_parse_from(["adnet", "config", "show"])?;
    match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Show => { println!("   ✓ ConfigCmd::Show -> Show"); }
            _ => {}
        },
        _ => {}
    }

    // ConfigCmd::Validate
    let cli = Cli::try_parse_from(["adnet", "config", "validate"])?;
    match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Validate => { println!("   ✓ ConfigCmd::Validate -> Validate"); }
            _ => {}
        },
        _ => {}
    }

    // ConfigCmd::Edit
    let cli = Cli::try_parse_from(["adnet", "config", "edit"])?;
    match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Edit => { println!("   ✓ ConfigCmd::Edit -> Edit"); }
            _ => {}
        },
        _ => {}
    }

    // ConfigCmd::Reset { yes: true }
    let cli = Cli::try_parse_from(["adnet", "config", "reset", "--yes"])?;
    match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Reset { .. } => { println!("   ✓ ConfigCmd::Reset {{ yes: true }} -> Reset"); }
            _ => {}
        },
        _ => {}
    }

    // ConfigCmd::Set
    let cli = Cli::try_parse_from(["adnet", "config", "set", "log.level", "debug"])?;
    match &cli.cmd {
        Cmd::Config { sub } => match sub {
            ConfigCmd::Set { .. } => { println!("   ✓ ConfigCmd::Set {{ ... }} -> Set"); }
            _ => {}
        },
        _ => {}
    }

    // 5. Diagnostics
    println!("\n5. Diagnostics command:");
    let cli = Cli::try_parse_from(["adnet", "diagnostics", "--json"])?;
    if let Cmd::Diagnostics { json } = &cli.cmd {
        println!("   ✓ Diagnostics parsed");
        println!("   JSON output: {json}");
    }

    // 6. IrohOpt parsing
    println!("\n6. IrohOpt parsing:");
    let iroh_values = [
        ("auto", IrohOpt::Auto),
        ("yes", IrohOpt::Yes),
        ("no", IrohOpt::No),
        ("true", IrohOpt::Yes),
        ("false", IrohOpt::No),
        ("1", IrohOpt::Yes),
        ("0", IrohOpt::No),
    ];

    for (input, expected) in iroh_values {
        let parsed: IrohOpt = input.parse().unwrap();
        assert_eq!(parsed, expected);
        println!("   '{input}' -> {expected:?}");
    }

    // 7. parse_iroh_opt utility
    println!("\n7. Using parse_iroh_opt():");
    let cases = [
        (None, IrohOpt::Auto),
        (Some("auto"), IrohOpt::Auto),
        (Some("yes"), IrohOpt::Yes),
        (Some("no"), IrohOpt::No),
    ];
    for (input, expected) in cases {
        let result = adnet_cli::cli::parse_iroh_opt(input)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        assert_eq!(result, expected);
        println!("   {:?} -> {:?}", input, result);
    }

    // 8. Error handling
    println!("\n8. Error handling:");
    let invalid = "".parse::<IrohOpt>();
    assert!(invalid.is_err());
    println!("   ✓ Empty string correctly rejected");

    let invalid = "maybe".parse::<IrohOpt>();
    assert!(invalid.is_err());
    println!("   ✓ Unknown value correctly rejected");

    println!("\n=== CLI Parser Demo Complete ===");
    Ok(())
}
