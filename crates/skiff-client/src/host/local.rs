//! M1a–M2 connection ownership: status, screen, inventory, session updates,
//! the session terminal, session lifecycle and groups, and the Host identity
//! skiffd reports in `pong`, over this Mac's socket or an SSH forward to a
//! remote skiffd. No Host service and no new skiffd requests: the adapter
//! speaks skiffd's own protocol over either transport.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use skiff_host_api::{
    Capability, Endpoint, Error, Hello, Host, HostId, InstanceId, Limits, Request, Response,
    API_MAJOR,
};
use uuid::Uuid;

use super::ssh::{self, SshError, SshForward, SshOptions, SshTarget};
use super::{
    AgentSource, GroupSource, HostConnection, InventorySource, LifecycleSource, LocalSources, OutputGap,
    OutputStream, ScreenSource, SessionUpdates, StatusSource, TerminalOutput, TerminalSource,
    UpdateGap, UpdateSource, UpdateStream,
};
use crate::{Client, DaemonInfo};

mod selection;
mod legacy;

pub use selection::{HostSelection, IdentitySource, PendingConnection, ProcessControl, SelectedHost, REMOTE_INPUT_LIMIT};
pub use legacy::{connection_status, legacy_agents, legacy_attach, legacy_create, legacy_delete_group, legacy_detach, legacy_groups, legacy_input, legacy_inventory, legacy_kill, legacy_order_groups, legacy_output, legacy_rename, legacy_resize, legacy_save_group, legacy_screen, legacy_seen, legacy_set_theme, legacy_updates, LegacyUpdates};
