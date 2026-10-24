//! sap-flux-monitor library: everything except the eframe entry point,
//! exposed so integration tests can drive the pipeline, logger, simulator,
//! and control logic directly.

pub mod app;
pub mod channels;
pub mod control;
pub mod history;
pub mod logger;
pub mod sim;
pub mod stats;
pub mod thermo;
pub mod types;
pub mod worker;
