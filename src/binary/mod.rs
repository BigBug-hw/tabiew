//! Raw binary / Parquet / Lance structure inspection.
//!
//! Pure logic, no UI dependencies — see `src/tui/hex_viewer.rs` for the view.
pub mod hex;
pub mod lance;
pub mod layout;
pub mod parquet;
pub mod parquet_decode;
pub mod protobuf;
pub mod source;
pub mod thrift;

mod parquet_schema;

mod lance_schema;
