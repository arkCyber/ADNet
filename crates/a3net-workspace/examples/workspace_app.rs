//! Realistic example: open a workspace, publish two files with
//! different content hashes, then read back the manifest and
//! confirm both files are listed, and that the gossip topic name
//! matches the `a3net-room-…` convention.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-workspace --example workspace_app
//! ```

use a3net_workspace::{Workspace, split_name_ext, workspace_room_topic};
use std::fs;
use std::io::Write;
use std::path::Path;

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    let mut f = fs::File::create(&p).expect("create");
    f.write_all(bytes).expect("write");
    p
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let staging = tempfile::tempdir()?;
    let ws = Workspace::new(dir.path(), "demo-node")?;

    // 1. Publish two files into shared/.
    let readme = write_file(staging.path(), "README.md", b"# Hello\nWorkspace demo\n");
    let report = write_file(staging.path(), "report.pdf", &[0x25, 0x50, 0x44, 0x46, 0x00]);

    let e1 = ws.publish_file(&readme, Some("blake3:aaa111".into()))?;
    let e2 = ws.publish_file(&report, Some("blake3:bbb222".into()))?;
    println!("published: {} ({} bytes)", e1.name, e1.size_bytes);
    println!("published: {} ({} bytes)", e2.name, e2.size_bytes);

    // 2. Confirm both files are now under shared/.
    let shared = ws.shared_dir();
    let names: Vec<String> = fs::read_dir(&shared)?
        .filter_map(|e| e.ok().map(|d| d.file_name().to_string_lossy().into_owned()))
        .collect();
    println!("shared/: {:?}", names);
    assert_eq!(names.len(), 2);

    // 3. Manifest snapshot round-trips through JSON.
    let m = ws.manifest_snapshot()?;
    assert_eq!(m.files.len(), 2);
    let json = serde_json::to_string_pretty(&m)?;
    println!("manifest:\n{json}");
    assert!(json.contains("blake3:aaa111"));
    assert!(json.contains("blake3:bbb222"));

    // 4. Topic name follows the `a3net-room-…` convention.
    let topic = workspace_room_topic();
    println!("gossip topic: {topic}");
    assert!(topic.starts_with("a3net-room-"));

    // 5. split_name_ext helper.
    let (stem, ext) = split_name_ext("report.pdf");
    assert_eq!(stem, "report");
    assert_eq!(ext, "pdf");
    let (stem2, ext2) = split_name_ext("README");
    assert_eq!(stem2, "README");
    assert_eq!(ext2, "");
    println!("split_name_ext: ({stem:?}, {ext:?}) ({stem2:?}, {ext2:?})");

    println!("ok");
    Ok(())
}