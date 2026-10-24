//! Bench tooling for the sap-flux board's field mode (docs/field-protocol.md
//! §1) without the gateway: a wake/retry command [`link`] over a serial
//! transport, [`window`] download, verification and CSV export, and
//! [`raw`] frame-log download.

pub mod link;
pub mod raw;
pub mod serial;
pub mod window;
