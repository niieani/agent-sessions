#![allow(dead_code)]
use agent_sessions::archive::Archive;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
pub fn capture(input: &Path, id: &str, url: &str, payload: Value, day: u32) {
    fs::create_dir_all(input).unwrap();
    let bytes = serde_json::to_vec(&payload).unwrap();
    fs::write(input.join(format!("{id}.json")), &bytes).unwrap();
    fs::write(input.join(format!("{id}.meta.json")),serde_json::to_vec(&json!({"accountId":"fixture-account","format":"chatgpt-web-response-v1","id":id,"url":format!("https://chatgpt.com/backend-api/{url}"),"capturedAt":format!("2026-10-{day:02}T12:00:00Z"),"sha256":format!("{:x}",Sha256::digest(&bytes))})).unwrap()).unwrap();
}
pub fn directory(input: &Path, id: &str, name: &str, day: u32) {
    capture(
        input,
        &format!("directory-{id}-{day}"),
        "gizmos/snorlax/sidebar?limit=20",
        json!({"items":[{"gizmo":{"gizmo":{"id":id,"display":{"name":name}}},"conversations":{"items":[],"cursor":null}}],"cursor":null}),
        day,
    );
}
pub fn detail(input: &Path, id: &str, project: Value, day: u32) {
    capture(
        input,
        &format!("detail-{id}-{day}"),
        &format!("conversations/{id}?num_turns=10"),
        json!({"update_time":1,"title":id,"gizmo_id":project,"messages":[{"id":format!("u-{id}"),"author":{"role":"user"},"create_time":1780000000,"content":{"content_type":"text","parts":["Original words"]}}],"page_info":{"has_previous_page":false}}),
        day,
    );
}
pub fn config(archive: &Archive, extra: &str) {
    fs::write(archive.root().join("config.toml"),format!("[views]\ntimezone=\"UTC\"\n[views.layout]\njson=\"json/{{{{short_id}}}}.json\"\n{extra}")).unwrap();
}
