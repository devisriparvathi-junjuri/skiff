//! M1a–M2 connection ownership: status, screen, inventory, session updates,
//! the session terminal, session lifecycle and groups, and the Host identity
//! skiffd reports in `pong`, over this Mac's socket or an SSH forward to a
//! remote skiffd. No Host service and no new skiffd requests: the adapter
//! speaks skiffd's own protocol over either transport.
//!
//! This module is the compatibility facade. Endpoint selection and the
//! connection's lifetime live in [`selection`]; the legacy sources and the
//! procedural adapter live in [`legacy`].

mod legacy;
mod selection;

pub use legacy::{connection_status, legacy_agents, legacy_attach, legacy_create, legacy_delete_group, legacy_detach, legacy_groups, legacy_input, legacy_inventory, legacy_kill, legacy_order_groups, legacy_output, legacy_rename, legacy_resize, legacy_save_group, legacy_screen, legacy_seen, legacy_set_theme, legacy_updates, LegacyUpdates};
pub use selection::{HostSelection, IdentitySource, PendingConnection, ProcessControl, REMOTE_INPUT_LIMIT, SelectedHost};
