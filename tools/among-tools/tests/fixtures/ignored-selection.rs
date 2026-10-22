use std::{fs, process::Command};

fn record(name: &str) {
    fs::write(
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(name),
        b"invoked",
    )
    .unwrap();
}

#[test]
#[ignore]
fn ordinary_standalone_probe() {
    record("ordinary-ran");
    assert!(std::env::var("HOME")
        .unwrap()
        .contains("among-rust-display-"));
    let output = Command::new("/usr/bin/xprop")
        .args(["-root", "_NET_SUPPORTING_WM_CHECK"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("window id"));
    assert!(
        !cfg!(ordinary_failure),
        "ordinary probe failed deliberately"
    );
}

mod plugin {
    pub mod install {
        pub mod source {
            pub mod tests {
                pub mod installation_authority {
                    #[test]
                    #[ignore]
                    fn cooperating_reinstall_worker() {
                        crate::record("worker-ran");
                        panic!("parent-owned selection fixture must never run standalone");
                    }

                    #[test]
                    #[ignore]
                    fn cooperating_reinstall_worker_suffix() {
                        crate::record("near-name-ran");
                    }
                }
            }
        }
    }
}
