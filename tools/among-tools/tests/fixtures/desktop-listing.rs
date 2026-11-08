//! Listing and executable-identity fixture. Its desktop declaration must never
//! impersonate the application's real VTE/GIO acceptance test.

#[cfg(snapshot_image)]
fn main() {
    let image = if cfg!(replacement_image) {
        "replacement"
    } else {
        "original"
    };
    let phase = if std::env::args().any(|arg| arg == "--list") {
        "listing"
    } else {
        "execution"
    };
    println!("{image}:{phase}");
}

#[test]
#[ignore]
fn ordinary_graphical_case() {
    panic!("the listing fixture cannot perform graphical acceptance");
}

#[cfg(not(missing_desktop))]
mod app {
    mod desktop_notification_tests {
        #[test]
        #[ignore]
        fn real_bells_send_exact_desktop_payloads_and_clicks_focus_only_current_targets() {
            panic!("the listing fixture cannot perform desktop notification acceptance");
        }
    }
}
