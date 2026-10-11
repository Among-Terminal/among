#[test]
fn native_uri_receipt_preserves_argument_bytes_and_refuses_unowned_paths() -> among_tools::Result<()>
{
    use std::fs;
    let base = among_tools::storage::private_directory(&std::env::temp_dir(), "among-menu-uri")?;
    let output = base.join("received.jsonl");
    let uri = "https://example.test/path?a=1&b=2";
    among_tools::menu_launch::record(&output, &[uri.into()])?;
    among_tools::menu_launch::record(&output, &["mailto:demo@example.test".into()])?;
    let bytes = fs::read_to_string(&output)?;
    let rows = bytes
        .lines()
        .map(|line| among_tools::storage::parse_json(line.as_bytes()))
        .collect::<among_tools::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        vec![
            serde_json::json!([uri]),
            serde_json::json!(["mailto:demo@example.test"])
        ]
    );
    assert!(among_tools::menu_launch::record(&base.join("foreign.jsonl"), &[uri.into()]).is_err());
    let outside = std::env::temp_dir().join("foreign-received.jsonl");
    fs::remove_file(&output)?;
    std::os::unix::fs::symlink(&outside, &output)?;
    assert!(among_tools::menu_launch::record(&output, &[uri.into()]).is_err());
    fs::remove_file(output)?;
    fs::remove_file(base.join("receipt.lock"))?;
    fs::remove_dir(base)?;
    Ok(())
}
