//! sap-flux-monitor: native desktop monitor for the sap-flux instrument.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use sap_flux_monitor::app;

fn main() -> eframe::Result<()> {
    // Captures live in the crate dir in development
    // (software/sap-flux-monitor/captures/, gitignored); fall back to the
    // current directory when installed elsewhere.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let captures_root = if manifest_dir.exists() {
        manifest_dir.join("captures")
    } else {
        std::path::PathBuf::from("captures")
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("sap-flux monitor"),
        ..Default::default()
    };
    eframe::run_native(
        "sap-flux monitor",
        options,
        Box::new(move |_cc| Ok(Box::new(app::MonitorApp::new(captures_root)))),
    )
}
