use among_tools::resources;
use serde_json::json;

#[test]
fn independent_accounting_and_original_tolerances_are_preserved() -> among_tools::Result<()> {
    assert_eq!(
        resources::cpu_counters("cpu  1 2 3 4 5 6 7 8 9\ncpu0 1")?,
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(
        resources::cpu_percent(&[0; 8], &[10, 0, 0, 80, 10, 0, 0, 0])?,
        10.0
    );
    assert_eq!(
        resources::memory_percent("MemTotal: 1000 kB\nMemAvailable: 800 kB\n")?,
        20.0
    );
    assert_eq!(
        resources::disk_percent(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\nroot 100 30 70 30% /\n"
        )?,
        30.0
    );
    let fragments = [
        json!({"text":"CPU 12%", "meter":0.12}),
        json!({"text":"MEM 18%", "meter":0.18}),
        json!({"text":"DISK 31%", "meter":0.31}),
    ];
    assert_eq!(
        resources::compare(&fragments, &[10.0, 20.0, 30.0])?,
        [12, 18, 31]
    );
    assert!(resources::compare(&fragments, &[9.9, 20.0, 30.0]).is_err());
    assert!(resources::compare(&fragments, &[10.0, 20.1, 30.0]).is_err());
    assert!(resources::compare(&fragments, &[10.0, 20.0, 29.9]).is_err());
    let mut changed = fragments.clone();
    changed[0]["meter"] = json!(0.13);
    assert!(resources::compare(&changed, &[10.0, 20.0, 30.0]).is_err());
    Ok(())
}

#[test]
fn malformed_or_invalid_observations_are_not_success() -> among_tools::Result<()> {
    assert!(resources::cpu_counters("cpu 1 2").is_err());
    assert!(resources::cpu_percent(&[0; 8], &[0; 8]).is_err());
    assert!(resources::cpu_percent(&[1; 8], &[0; 8]).is_err());
    for memory in [
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 10 kB\nMemAvailable: 20 kB",
        "MemTotal: 10 kB",
        "MemTotal: 10 kB\nMemTotal: 10 kB\nMemAvailable: 1 kB",
    ] {
        assert!(resources::memory_percent(memory).is_err());
    }
    assert!(resources::disk_percent("root 100 30 70 30% /home").is_err());
    let response = json!({"request_id":2,"fragments":[{},{},{}]});
    assert!(resources::response(&serde_json::to_vec(&response)?, 1).is_err());
    assert_eq!(
        resources::response(&serde_json::to_vec(&response)?, 2)?.len(),
        3
    );
    assert!(resources::response(b"{malformed}", 1).is_err());
    assert!(resources::response(br#"{"request_id":1,"fragments":[]}"#, 1).is_err());
    Ok(())
}

#[test]
fn actual_cli_refuses_failed_and_missing_plugins_without_a_pass_receipt() -> among_tools::Result<()>
{
    for binary in ["/bin/false", "/missing-resource-plugin"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .args(["check-resource-measurements", binary])
            .output()?;
        assert!(!output.status.success());
        assert!(!String::from_utf8(output.stdout)?.contains("PASS"));
    }
    Ok(())
}
