use agent_sessions::{archive::Archive, render::render_all};
use std::fs;

#[test]
fn t3_worktrees_are_valid_view_outputs_but_userdata_is_protected() {
    let dir = tempfile::tempdir().unwrap();
    let archive = Archive::open(dir.path()).unwrap();
    archive.db().unwrap();
    let t3 = dir.path().join("t3");
    fs::create_dir_all(t3.join("userdata")).unwrap();
    for (relative, permitted) in [("worktrees/review/views", true), ("userdata/views", false)] {
        fs::write(
            dir.path().join("config.toml"),
            format!(
                "[[host]]\nname='local'\nt3-root={:?}\n[views]\noutput={:?}\n",
                t3.to_str().unwrap(),
                t3.join(relative).to_str().unwrap()
            ),
        )
        .unwrap();
        let result = render_all(&archive);
        assert_eq!(result.is_ok(), permitted, "{result:?}");
    }
}
