//! Demo: create a workspace and publish a file.

use adnet_workspace::{Workspace, workspace_room_topic};
use std::io::Write;

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = Workspace::new(dir.path(), "demo-node").expect("workspace");
    println!("root: {}", ws.root().display());
    println!("shared: {}", ws.shared_dir().display());
    println!("inbox: {}", ws.inbox_dir().display());
    println!("outbox: {}", ws.outbox_dir().display());
    println!("topic: {}", workspace_room_topic());

    let src = dir.path().join("demo.txt");
    let mut f = std::fs::File::create(&src).expect("create");
    writeln!(f, "demo file").expect("write");
    let entry = ws
        .publish_file(&src, Some("deadbeef".into()))
        .expect("publish");
    println!("published: {:?}", entry);

    let m = ws.manifest_snapshot().expect("manifest");
    println!("manifest: {}", serde_json::to_string_pretty(&m).unwrap());
}
