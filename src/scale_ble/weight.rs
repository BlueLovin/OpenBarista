//! Re-exports weight parsing from the library crate so that other `scale_ble`
//! sub-modules can use `weight::parse_weight` unchanged.

pub use openbarista::scale_weight::parse_weight;
