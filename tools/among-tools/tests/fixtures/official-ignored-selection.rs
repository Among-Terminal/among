use std::{fs, path::PathBuf};

fn record(name: &str, bytes: &[u8]) {
    fs::write(std::env::current_exe().unwrap().parent().unwrap().join(name), bytes).unwrap();
}

fn ordinary(name: &str) {
    assert_eq!(std::env::var("GTK_A11Y").unwrap(), "none");
    assert!(std::env::var_os("AMONG_OFFICIAL_SOURCE_CAPTURES").is_none());
    record(name, b"ordinary route");
}

mod plugin {
    pub mod install {
        pub mod registry {
            pub mod tests {
                pub mod official_source {
                    #[test]
                    #[ignore]
                    fn effective_sources_fresh_process_worker() {
                        crate::record("official-worker-ran", b"misrouted");
                        panic!("parent-owned worker must not run standalone");
                    }

                    #[test]
                    #[ignore]
                    fn effective_sources_fresh_process_worker_suffix() {
                        crate::ordinary("official-worker-near-name-ran");
                    }
                }
            }
        }
    }
}

mod prefs {
    pub mod repository_sources {
        pub mod tests {
            #[test]
            #[ignore]
            fn official_row_survives_offline_refresh_custom_removal_reopen_and_corrupt_storage() {
                assert_eq!(std::env::var("GTK_A11Y").unwrap(), "test");
                let captures = crate::PathBuf::from(std::env::var_os("AMONG_OFFICIAL_SOURCE_CAPTURES").unwrap());
                assert!(captures.is_absolute() && captures.starts_with(std::env::temp_dir()));
                assert!(captures.starts_with(std::env::var_os("AMONG_CHECK_ROOT").unwrap()));
                assert!(!crate::fs::symlink_metadata(&captures).unwrap().file_type().is_symlink());
                let capture = captures.join("route.txt");
                crate::fs::write(&capture, b"observed exact owned capture route").unwrap();
                crate::record("official-capture-path", capture.to_str().unwrap().as_bytes());
            }

            #[test]
            #[ignore]
            fn official_row_survives_offline_refresh_custom_removal_reopen_and_corrupt_storage_suffix() {
                crate::ordinary("official-widget-near-name-ran");
            }
        }
    }
}
