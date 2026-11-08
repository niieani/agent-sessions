#[path = "support/chatgpt.rs"]
mod shared;
use agent_sessions::{
    chatgpt::{Evidence, Graph},
    render::render_all,
};
use serde_json::json;
#[test]
fn capturing_host_is_not_an_origin_host_in_views() {
    let graph=Graph::from_evidence("c", &[Evidence {format:"browser".into(),origin:"https://chatgpt.com/backend-api/conversations/c".into(),captured_at:1,sha256:"fixture".into(),payload:json!({"update_time":1,"messages":[{"id":"u","author":{"role":"user"},"create_time":1770000000,"content":{"parts":["hello"]}}]}).to_string()}]).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let archive = shared::graph_archive(&graph, temp.path());
    std::fs::write(
        archive.root().join("host.j2"),
        "origin=[{{ session.host }}] capture=[{{ session.capture_host }}]\n{{ body }}",
    )
    .unwrap();
    std::fs::write(
        archive.root().join("config.toml"),
        format!(
            "[views]\ntemplate={:?}\n",
            archive.root().join("host.j2").to_str().unwrap()
        ),
    )
    .unwrap();
    render_all(&archive).unwrap();
    assert!(shared::text(&archive).contains("origin=[]"));
}
