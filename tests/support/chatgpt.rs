#![allow(dead_code)]
use agent_sessions::{archive::Archive, chatgpt::Graph, model::Provenance};
use std::path::{Path, PathBuf};
pub fn transcripts(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            paths.extend(transcripts(&p));
        } else if p.extension().is_some_and(|ext| ext == "md")
            && p.file_name().unwrap() != "INDEX.md"
        {
            paths.push(p);
        }
    }
    paths
}
pub fn text(archive: &Archive) -> String {
    transcripts(&archive.views_dir())
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect()
}
pub fn graph_archive(graph: &Graph, path: &Path) -> Archive {
    let archive = Archive::open(path).unwrap();
    let mut conversation = graph
        .conversation(Provenance {
            source: "chatgpt".into(),
            host: "fixture".into(),
            native_id: graph.id.clone(),
            native_path: "fixture".into(),
        })
        .unwrap();
    // Early synthetic graph fixtures predate timestamp-based standard Views.
    for message in conversation.messages.iter_mut().chain(
        conversation
            .branches
            .iter_mut()
            .flat_map(|b| b.messages.iter_mut()),
    ) {
        if message.timestamp_ms.is_none() {
            message.timestamp_ms = Some(1_770_000_000_000);
        }
    }
    archive
        .db()
        .unwrap()
        .upsert_conversation(&conversation)
        .unwrap();
    archive
}

pub fn collect(archive: &Archive, store: &Path) -> anyhow::Result<agent_sessions::pull::PullStats> {
    let missing = store.join("not-a-store");
    let roots = agent_sessions::pull::StoreRoots {
        claude: missing.clone(),
        codex: missing.clone(),
        pi: missing.clone(),
        cursor: missing.clone(),
        t3: missing,
        t3_explicit: false,
        chatgpt: store.to_path_buf(),
    };
    agent_sessions::pull::pull_localhost(archive, &roots, &agent_sessions::pull::local_hostname())
}

pub fn decode_href(href: &str) -> String {
    let mut bytes = Vec::new();
    let mut input = href.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex = [input.next().unwrap(), input.next().unwrap()];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap());
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).unwrap()
}
