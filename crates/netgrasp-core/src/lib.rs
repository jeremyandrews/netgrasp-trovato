//! Host-agnostic core for the Netgrasp network-monitoring plugin.
//!
//! Netgrasp's native daemon watches a LAN and writes `ng_`-prefixed tables into
//! Trovato's database. This plugin makes those rows visible and editable through
//! the kernel, which means two problems that are worth solving away from a
//! database: deciding **what a sync pass should do with a dirty row**, and
//! deciding **which columns each writer is allowed to touch**. Both live here as
//! pure functions.
//!
//! # Shape
//!
//! - [`model`] — the domain rows ([`model::DeviceRow`], [`model::PersonFields`],
//!   [`model::Span`], [`model::EventRow`]).
//! - [`columns`] — the three disjoint column sets that make "the two writers
//!   never collide" a checkable property rather than a promise.
//! - [`queries`] — every statement the plugin issues against the daemon's
//!   tables, hoisted here so a test can run the real ones against the daemon's
//!   own DDL.
//! - [`sync`] — the daemon→kernel plan: create, relink, refresh or skip.
//! - [`writeback`] — the kernel→daemon direction, including the loop-termination
//!   argument and the statement builder that cannot name a foreign column.
//! - [`retention`] — the event-pruning window.
//! - [`timeline`] — presence/location/IP spans collapsed into what a device page
//!   shows.
//! - [`error`] — [`error::CoreError`] and its transient/permanent split.
//!
//! Everything here is depended on by the `plugins/netgrasp` cdylib, which
//! supplies the host bindings. Keeping the core pure is what lets the sync
//! plan and the column discipline be tested exhaustively with no database and
//! no wasm host in sight.

#![forbid(unsafe_code)]

pub mod columns;
pub mod error;
pub mod model;
pub mod queries;
pub mod retention;
pub mod sync;
pub mod timeline;
pub mod writeback;

pub use error::{CoreError, CoreResult};

/// Item content type for a device's user-owned overlay.
pub const DEVICE_TYPE: &str = "ng_device";

/// Item content type for a person.
pub const PERSON_TYPE: &str = "ng_person";

/// Record type name for the daemon-owned device state.
///
/// Deliberately **not** `ng_device`: a record type name shares the gather
/// `item_type` slot with content types and is rejected if it collides with one
/// (`RecordTypeRegistry::admit`). The device is two tiers, so it needs two names.
pub const DEVICE_STATE_RECORD: &str = "ng_device_state";

/// Record type name for events.
pub const EVENT_RECORD: &str = "ng_event";
