mod plugin {
    pub mod install {
        pub mod store {
            pub mod tests {
                #[test]
                #[ignore = "enumeration fixture only; never real ENOSPC acceptance"]
                fn real_enospc_before_selection_keeps_previous_installation_and_never_reports_success(
                ) {
                    panic!("selection fixture must never count as real ENOSPC execution");
                }
            }
        }
    }
}
