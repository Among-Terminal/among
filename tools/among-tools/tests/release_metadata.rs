use among_tools::{release_metadata, Result};
use serde_json::{json, Value};

const XML:&str="<component><id>com.github.among-terminal</id><name>Among Terminal</name><project_license>Apache-2.0</project_license><launchable>com.github.among-terminal.desktop</launchable><releases><release version=\"0.1.0\"/></releases></component>";
const DESKTOP:&str="[Desktop Entry]\nName=Among Terminal\nExec=among\nIcon=com.github.among-terminal\nTerminal=false\n";
fn fixture() -> Value {
    json!({"name":"among","base":"core24","confinement":"classic","grade":"devel","license":"Apache-2.0","platforms":{"amd64":{"build-on":["amd64"],"build-for":["amd64"]},"arm64":{"build-on":["arm64"],"build-for":["arm64"]}},"apps":{"among":{"command":"bin/among"}},"adopt-info":"among","parts":{"among":{"rust-channel":"1.98.1","rust-path":[".","examples/plugins"],"rust-inherit-ldflags":true,"build-environment":[{"LDFLAGS":"-L/usr/lib/$CRAFT_ARCH_TRIPLET_BUILD_FOR"}],"stage-packages":["librsvg2-common"],"build-attributes":["enable-patchelf"],"override-build":"true\n","override-stage":"glib-compile-schemas --strict \"$CRAFT_PART_INSTALL/usr/share/glib-2.0/schemas\"\n"}}})
}
fn manifests() -> Result<[toml::Value; 2]> {
    let document: toml::Value =
        toml::from_str("[package]\nversion=\"0.1.0\"\nlicense=\"Apache-2.0\"\n")?;
    Ok([document.clone(), document])
}

#[test]
fn each_original_recipe_consistency_boundary_refuses_mutated_effective_values() -> Result<()> {
    let valid = fixture();
    let manifests = manifests()?;
    assert_eq!(
        release_metadata::validate(&valid, &manifests, XML, DESKTOP)?.len(),
        2
    );
    for mutation in 0..16 {
        let mut changed = valid.clone();
        match mutation {
            0 => changed["name"] = "other".into(),
            1 => changed["base"] = "core26".into(),
            2 => changed["confinement"] = "strict".into(),
            3 => changed["grade"] = "stable".into(),
            4 => changed["license"] = "MIT".into(),
            5 => {
                changed["platforms"]
                    .as_object_mut()
                    .ok_or("fixture platforms absent")?
                    .remove("arm64");
            }
            6 => changed["apps"]["among"]["command"] = "other".into(),
            7 => changed["adopt-info"] = "other".into(),
            8 => changed["parts"]["among"]["rust-channel"] = "stable".into(),
            9 => changed["parts"]["among"]["rust-path"] = json!(["."]),
            10 => changed["parts"]["among"]["rust-cargo-parameters"] = json!(["--locked"]),
            11 => changed["parts"]["among"]["rust-inherit-ldflags"] = false.into(),
            12 => {
                changed["parts"]["among"]["build-environment"][0]["LDFLAGS"] =
                    "--dynamic-linker /other".into()
            }
            13 => changed["parts"]["among"]["override-stage"] = "x86_64-linux-gnu".into(),
            14 => changed["parts"]["among"]["stage-packages"] = json!([]),
            15 => changed["parts"]["among"]["override-prime"] = "true".into(),
            _ => return Err("unknown fixture mutation".into()),
        }
        assert!(
            release_metadata::validate(&changed, &manifests, XML, DESKTOP).is_err(),
            "mutation {mutation}"
        );
    }
    let mut changed = valid.clone();
    changed["parts"]["among"]["build-attributes"] = json!([]);
    assert!(release_metadata::validate(&changed, &manifests, XML, DESKTOP).is_err());
    let mut changed = valid;
    changed["parts"]["among"]["override-stage"] = "true".into();
    assert!(release_metadata::validate(&changed, &manifests, XML, DESKTOP).is_err());
    Ok(())
}

#[test]
fn manifest_xml_desktop_version_license_and_identity_must_agree() -> Result<()> {
    let valid = fixture();
    let manifests = manifests()?;
    for (key, value) in [("version", "0.2.0"), ("license", "MIT")] {
        let mut changed = manifests.clone();
        changed[1]["package"][key] = toml::Value::String(value.into());
        assert!(release_metadata::validate(&valid, &changed, XML, DESKTOP).is_err());
    }
    for xml in [
        XML.replace("0.1.0", "0.2.0"),
        XML.replace("Apache-2.0", "MIT"),
        XML.replace("com.github.among-terminal", "other"),
        XML.replace("Among Terminal", "Other name"),
    ] {
        assert!(release_metadata::validate(&valid, &manifests, &xml, DESKTOP).is_err());
    }
    for desktop in [
        DESKTOP.replace("Exec=among", "Exec=/usr/bin/among"),
        DESKTOP.replace("Icon=com.github.among-terminal", "Icon=other"),
        DESKTOP.replace("Terminal=false", "Terminal=true"),
        DESKTOP.replace("Among Terminal", "Other"),
        format!("{DESKTOP}Exec=among\n"),
        format!("{DESKTOP}[Desktop Entry]\n"),
    ] {
        assert!(release_metadata::validate(&valid, &manifests, XML, &desktop).is_err());
    }
    Ok(())
}
