//! Bench tooling for the sap-flux board's field mode (docs/field-protocol.md
//! §1) without the gateway: a wake/retry command [`link`] over a serial
//! transport, and [`window`] download, verification and CSV export.

pub mod link;
pub mod serial;
pub mod window;
