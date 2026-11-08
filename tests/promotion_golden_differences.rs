//! Store-to-View integration tests verify these current fixtures. This test
//! constrains their complete byte differences from the 0.2.3 baselines.
use serde_json::Value;
use std::{fs, path::Path};
#[test]
fn full_goldens_differ_only_in_confirmed_origin_hosts_and_archived_media_links() {
    let baseline: Value =
        serde_json::from_str(include_str!("goldens/promotion_0_2_3.json")).unwrap();
    let mut changes = (0, 0, 0);
    for (key, old) in baseline["files"].as_object().unwrap() {
        let (source, path) = key.split_once('/').unwrap();
        let actual = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(source)
                .join("expected")
                .join(path),
        )
        .unwrap();
        let old = old.as_str().unwrap();
        let mut expected = if source == "t3" {
            old.to_owned()
        } else {
            old.replace("\nhost: testhost\n", "\nhost: \"\"\n")
        };
        if expected != old {
            changes.1 += 1;
        }
        if key == "claude/webapp/2026-03-06T220100Z.0a7ac588.claude.md" {
            expected=expected.replace("[image]","![Image](../../media/sha256/4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6.png)");
            changes.2 += 1;
        }
        assert_eq!(actual, expected, "unexpected promotion difference in {key}");
        if actual != old {
            changes.0 += 1;
        }
    }
    assert_eq!(
        changes,
        (8, 8, 1),
        "changed files, Host fields, media links"
    );
}
