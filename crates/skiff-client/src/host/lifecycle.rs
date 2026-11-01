//! M1g lifecycle and group boundary. Exhaustive mappings, as in inventory:
//! a new core field or variant is a compile error until projected here.
use skiff_core::{group as core_group, project::AgentInfo, session as core};
use skiff_host_api::{groups as api_group, lifecycle as api, Error, HostAgent};

use super::inventory::{project_role, restore_role};

/// The desktop's spec, as the Host contract carries it. `coordinator` is never
/// on the legacy wire (`serde(skip)`); a spec that sets it, or a project
/// Skipper's mark, is refused here rather than silently changed.
pub(super) fn project_spec(value: core::SessionSpec) -> Result<api::SessionSpec, Error> {
    let core::SessionSpec {
        label,
        name,
        role,
        cwd,
        command,
        args,
        env,
        peek,
        coordinator,
        skipper,
        cols,
        rows,
    } = value;
    // A task type's variables go to skiffd through `skiff new --type` only.
    if !env.is_empty() {
        return Err(Error::MalformedContract {
            message: "variables for a session start only through skiff new --type".into(),
        });
    }
    if coordinator {
        return Err(Error::MalformedContract {
            message: "a coordinator session starts only through the coordinator request".into(),
        });
    }
    if skipper.is_some() {
        return Err(Error::MalformedContract {
            message: "a project Skipper starts only through the skipper_project request".into(),
        });
    }
    Ok(api::SessionSpec {
        label,
        name,
        role: project_role(role),
        cwd: cwd.map(super::inventory::opaque_path).transpose()?,
        command,
        args,
        peek,
        cols,
        rows,
    })
}

pub(super) fn restore_spec(value: api::SessionSpec) -> core::SessionSpec {
    let api::SessionSpec {
        label,
        name,
        role,
        cwd,
        command,
        args,
        peek,
        cols,
        rows,
    } = value;
    core::SessionSpec {
        label,
        name,
        role: restore_role(role),
        cwd: cwd.map(Into::into),
        command,
        args,
        env: Vec::new(),
        peek,
        coordinator: false,
        skipper: None,
        cols,
        rows,
    }
}

pub(super) fn project_group(value: core_group::Group) -> api_group::Group {
    let core_group::Group {
        id,
        name,
        layout,
        focus,
        cwd,
    } = value;
    api_group::Group {
        id,
        name,
        layout: project_layout(layout),
        focus,
        cwd,
    }
}

pub(super) fn restore_group(value: api_group::Group) -> core_group::Group {
    let api_group::Group {
        id,
        name,
        layout,
        focus,
        cwd,
    } = value;
    core_group::Group {
        id,
        name,
        layout: restore_layout(layout),
        focus,
        cwd,
    }
}

fn project_layout(value: core_group::Layout) -> api_group::Layout {
    match value {
        core_group::Layout::Pane { session } => api_group::Layout::Pane { session },
        core_group::Layout::Split { dir, ratio, a, b } => api_group::Layout::Split {
            dir: match dir {
                core_group::SplitDir::Row => api_group::SplitDir::Row,
                core_group::SplitDir::Col => api_group::SplitDir::Col,
            },
            ratio,
            a: Box::new(project_layout(*a)),
            b: Box::new(project_layout(*b)),
        },
    }
}

fn restore_layout(value: api_group::Layout) -> core_group::Layout {
    match value {
        api_group::Layout::Pane { session } => core_group::Layout::Pane { session },
        api_group::Layout::Split { dir, ratio, a, b } => core_group::Layout::Split {
            dir: match dir {
                api_group::SplitDir::Row => core_group::SplitDir::Row,
                api_group::SplitDir::Col => core_group::SplitDir::Col,
            },
            ratio,
            a: Box::new(restore_layout(*a)),
            b: Box::new(restore_layout(*b)),
        },
    }
}

/// M3: one agent of the Host account's list, as skiffd reports it.
pub fn project_agent(value: AgentInfo) -> HostAgent {
    let AgentInfo {
        id,
        command,
        default_command,
        custom,
        installed,
        enabled,
        used_by,
    } = value;
    HostAgent {
        id,
        command,
        default_command,
        custom,
        installed,
        enabled,
        used_by,
    }
}

pub fn restore_agent(value: HostAgent) -> AgentInfo {
    let HostAgent {
        id,
        command,
        default_command,
        custom,
        installed,
        enabled,
        used_by,
    } = value;
    AgentInfo {
        id,
        command,
        default_command,
        custom,
        installed,
        enabled,
        used_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_in_use_says_so_through_the_host_contract() {
        let a = AgentInfo { id: "grok".into(), command: "grok".into(), default_command: "grok".into(), custom: false, installed: true, enabled: true, used_by: Some("Used by task type web-research".into()) };
        let wire = serde_json::to_value(project_agent(a.clone())).unwrap();
        assert_eq!(wire["used_by"], "Used by task type web-research");
        assert_eq!(restore_agent(serde_json::from_value(wire).unwrap()), a);
        // An older Host sends no line: the agent reads as free.
        let old: HostAgent = serde_json::from_value(serde_json::json!({ "id": "grok", "command": "grok", "installed": true, "enabled": true })).unwrap();
        assert_eq!(restore_agent(old).used_by, None);
    }
}
