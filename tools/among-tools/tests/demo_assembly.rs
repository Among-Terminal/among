use among_tools::{demo_assembly, storage, Result};
use serde_json::json;
use std::fs;

#[test]
fn full_speed_silent_concat_retains_original_sources_chapters_and_poster_formula() -> Result<()> {
    let manifest = json!({"files":[{"name":"appearance","title":"appearance","duration":4,"sha256":"a".repeat(64)},{"name":"splits","title":"splits","duration":4,"sha256":"b".repeat(64)}]});
    let (graph, chapters, duration, poster) = demo_assembly::plan(&manifest)?;
    assert!(graph.contains("setpts=PTS-STARTPTS"));
    assert!(graph.contains("concat=n=2:v=1:a=0"));
    assert!(!graph.contains("fade"));
    assert!(!graph.contains("drawtext"));
    assert_eq!(chapters[1]["start"], 4.0);
    assert_eq!(duration, 8.0);
    assert_eq!(poster, 10.0);
    assert_eq!(chapters[0]["source_sha256"], "a".repeat(64));
    for field in ["name", "title", "duration", "sha256"] {
        let mut changed = manifest.clone();
        changed["files"][0][field] = json!(null);
        assert!(demo_assembly::plan(&changed).is_err());
    }
    let mut unsafe_name = manifest.clone();
    unsafe_name["files"][0]["name"] = json!("../outside");
    assert!(demo_assembly::plan(&unsafe_name).is_err());
    let mut duplicate = manifest;
    duplicate["files"][1]["name"] = json!("appearance");
    assert!(demo_assembly::plan(&duplicate).is_err());
    Ok(())
}

#[test]
fn actual_assembly_cli_refuses_changed_input_before_encoding_or_output_creation() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-demo-assembly")?;
    let manifest = json!({"files":[{"name":"appearance","title":"appearance","duration":4,"sha256":"a".repeat(64)},{"name":"splits","title":"splits","duration":4,"sha256":"b".repeat(64)}]});
    fs::write(root.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
    fs::write(root.join("appearance.mp4"), b"unreviewed")?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("assemble-demo")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    assert_eq!(fs::read(root.join("appearance.mp4"))?, b"unreviewed");
    fs::remove_dir_all(root)?;
    Ok(())
}
