use among_tools::{native_notices, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    let base = storage::private_directory(&std::env::temp_dir(), "among-native-notices")?;
    fs::create_dir_all(base.join("prime/usr/share/doc/fixture"))?;
    fs::create_dir_all(base.join("prime/usr/share/common-licenses"))?;
    fs::write(
        base.join("prime/usr/share/doc/fixture/copyright"),
        b"authentic fixture notice\n",
    )?;
    fs::write(
        base.join("prime/usr/share/common-licenses/MIT"),
        b"fixture common license\n",
    )?;
    Ok(Fixture(base))
}
fn document() -> Result<Value> {
    Ok(
        json!({"schema":1,"packages":[{"package":"fixture","notices":[{"path":"notices/fixture/usr/share/doc/fixture/copyright","sha256":storage::sha256(b"authentic fixture notice\n".as_slice())?}],"common_license_references":["MIT","MIT"]}]}),
    )
}

#[test]
fn actual_cli_hashes_verbatim_payload_and_deduplicates_common_references() -> Result<()> {
    let f = fixture()?;
    let doc = document()?;
    let record = native_notices::check(&doc, &f.0.join("prime"))?;
    assert_eq!(record["notices"], 1);
    assert_eq!(
        record["common_licenses"]
            .as_array()
            .ok_or("licenses absent")?
            .len(),
        1
    );
    assert!(record["compliance"]
        .as_str()
        .ok_or("scope absent")?
        .contains("not established"));
    let input = f.0.join("native.json");
    fs::write(&input, serde_json::to_vec(&doc)?)?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-snap-notices")
        .arg(input)
        .arg(f.0.join("prime"))
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(storage::parse_json(&output.stdout)?, record);
    fs::write(
        f.0.join("prime/usr/share/doc/fixture/copyright"),
        b"changed",
    )?;
    assert!(native_notices::check(&doc, &f.0.join("prime")).is_err());
    Ok(())
}

#[test]
fn missing_empty_traversal_layout_and_escaping_links_fail_but_packaged_links_work() -> Result<()> {
    let f = fixture()?;
    let doc = document()?;
    let prime = f.0.join("prime");
    for name in [
        "notices/foreign/usr/share/doc/fixture/copyright",
        "notices/fixture/../outside",
        "notices/fixture//absolute",
    ] {
        let mut changed = doc.clone();
        changed["packages"][0]["notices"][0]["path"] = name.into();
        assert!(native_notices::check(&changed, &prime).is_err());
    }
    let mut changed = doc.clone();
    changed["packages"][0]["common_license_references"] = json!(["../MIT"]);
    assert!(native_notices::check(&changed, &prime).is_err());
    let copyright = prime.join("usr/share/doc/fixture/copyright");
    let text = fs::read(&copyright)?;
    fs::remove_file(&copyright)?;
    assert!(native_notices::check(&doc, &prime).is_err());
    fs::write(prime.join("usr/share/doc/fixture/original"), &text)?;
    std::os::unix::fs::symlink("original", &copyright)?;
    native_notices::check(&doc, &prime)?;
    fs::remove_file(&copyright)?;
    fs::write(f.0.join("foreign"), &text)?;
    std::os::unix::fs::symlink(f.0.join("foreign"), &copyright)?;
    assert!(native_notices::check(&doc, &prime).is_err());
    fs::remove_file(&copyright)?;
    fs::write(&copyright, text)?;
    fs::write(prime.join("usr/share/common-licenses/MIT"), b"")?;
    assert!(native_notices::check(&doc, &prime).is_err());
    assert!(native_notices::check(&json!({"schema":1,"packages":[]}), &prime).is_err());
    let mut changed = doc;
    changed["schema"] = 2.into();
    assert!(native_notices::check(&changed, &prime).is_err());
    Ok(())
}
