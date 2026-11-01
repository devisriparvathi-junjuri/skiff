use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use skiff_host_api::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/m0.json")).unwrap()
}
fn golden<T: DeserializeOwned + Serialize>(name: &str) {
    for value in fixture()[name].as_array().unwrap() {
        let decoded: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), *value, "{name}");
    }
}

#[test]
fn every_m0_shape_matches_golden_json() {
    golden::<Host>("hosts");
    golden::<Endpoint>("endpoints");
    golden::<Hello>("hellos");
    golden::<Capability>("capabilities");
    golden::<Limits>("limits");
    golden::<Request>("requests");
    golden::<Envelope>("envelopes");
    golden::<Response>("responses");
    golden::<Error>("errors");
    golden::<Message>("messages");
}

#[test]
fn binary_bytes_are_base64_not_utf8_and_order_survives() {
    let r: Request =
        serde_json::from_value(json!({"cmd":"terminal_input","session":"s1","data":"AP8KDQ=="}))
            .unwrap();
    assert_eq!(
        r,
        Request::TerminalInput {
            session: "s1".into(),
            data: vec![0, 255, 10, 13],
            paste: false
        }
    );
    for key in ["terminal_snapshot", "terminal_output"] {
        let e = json!({"event":key,"session":"s1","data":"not base64!","cols":80,"rows":24});
        assert!(serde_json::from_value::<Event>(e).is_err());
    }
}

#[test]
fn additive_metadata_and_future_capabilities_do_not_break_old_readers() {
    let mut value = fixture()["hellos"][0].clone();
    value["future_optional"] = json!({"anything":true});
    value["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"future.feature","version":3}));
    let hello: Hello = serde_json::from_value(value).unwrap();
    hello.validate().unwrap();
    hello.require(&Capability::connection_status()).unwrap();
    assert!(matches!(
        hello.require(&Capability {
            name: "files.read".into(),
            version: 1
        }),
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[test]
fn incompatible_major_invalid_identity_and_bad_limits_are_explicit() {
    let mut hello: Hello = serde_json::from_value(fixture()["hellos"][0].clone()).unwrap();
    hello.api_major = 2;
    assert_eq!(
        hello.validate(),
        Err(Error::IncompatibleVersion {
            supported: 1,
            received: 2
        })
    );
    hello.api_major = 1;
    hello.limits.max_in_flight = 0;
    assert!(matches!(
        hello.validate(),
        Err(Error::MalformedContract { .. })
    ));
    assert!(serde_json::from_value::<HostId>(json!("")).is_err());
    assert!(serde_json::from_value::<InstanceId>(json!(" ")).is_err());
    assert!(serde_json::from_value::<Hello>(json!({"api_major":1})).is_err());
}

#[test]
fn capability_majors_are_exact_and_service_versions_do_not_replace_negotiation() {
    let mut hello: Hello = serde_json::from_value(fixture()["hellos"][0].clone()).unwrap();
    hello.capabilities[0].version = 2;
    hello
        .service_versions
        .insert("skiff-host".into(), "900.0.0".into());
    hello.validate().unwrap();
    assert!(matches!(
        hello.require(&Capability::connection_status()),
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[test]
fn m1a_status_is_additive_and_gated_by_its_own_capability() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1a.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let response: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    let capability: Capability = serde_json::from_value(fixture["capability"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&response).unwrap(), fixture["reply"]);
    assert_eq!(request.capability(), Some(capability));
    assert!(request.accepts(&response));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::SessionInventory.accepts(&response));
}

#[test]
fn m1b_screen_has_an_independent_read_only_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1b.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let reply: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    let error: Error = serde_json::from_value(fixture["error"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&reply).unwrap(), fixture["reply"]);
    assert_eq!(serde_json::to_value(error).unwrap(), fixture["error"]);
    assert_eq!(
        serde_json::to_value(request.capability().unwrap()).unwrap(),
        fixture["capability"]
    );
    assert!(request.accepts(&reply));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::SessionInventory.accepts(&reply));
    let envelope = Envelope { id: 9, request };
    assert_eq!(
        serde_json::from_value::<Envelope>(serde_json::to_value(&envelope).unwrap()).unwrap(),
        envelope
    );
}

#[test]
fn m1c_inventory_golden_schema_preserves_defaults_nulls_omissions_and_metadata() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1c.json")).unwrap();
    let request: Request = serde_json::from_value(fixture["request"].clone()).unwrap();
    let response: Response = serde_json::from_value(fixture["reply"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), fixture["request"]);
    assert_eq!(serde_json::to_value(&response).unwrap(), fixture["reply"]);
    assert_eq!(
        serde_json::to_value(request.capability().unwrap()).unwrap(),
        fixture["capability"]
    );
    assert!(request.accepts(&response));
    assert!(!request.accepts(&Response::Ok));
    assert!(!Request::ConnectionStatus.accepts(&response));
    let minimal: InventorySession =
        serde_json::from_value(fixture["legacy_minimal"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(minimal).unwrap(),
        fixture["minimal_output"]
    );
    let mut malformed = fixture["legacy_minimal"].clone();
    malformed["role"] = "future-role".into();
    assert!(serde_json::from_value::<InventorySession>(malformed).is_err());
    let mut malformed = fixture["reply"]["sessions"][0].clone();
    malformed["watch"]["pids"] = json!(["bad"]);
    assert!(serde_json::from_value::<InventorySession>(malformed).is_err());
}

#[test]
fn m1d_session_updates_have_an_explicit_capability_envelope_and_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1d.json")).unwrap();
    assert_eq!(
        serde_json::to_value(Capability::session_updates()).unwrap(),
        fixture["capability"]
    );
    for value in fixture["messages"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
        let Message::Event { event } = message else {
            panic!("not an event")
        };
        assert_eq!(event.capability(), Capability::session_updates());
    }
    // Unknown kinds and states fail.
    for update in [
        json!({"kind":"future_update","session":"a"}),
        json!({"kind":"state","session":"a","state":"future"}),
        json!({"kind":"exit","session":"a","code":"x"}),
        json!({"session":"a"}),
    ] {
        let event = json!({"event":"session_update","update":update});
        assert!(serde_json::from_value::<Event>(event).is_err());
    }
}

#[test]
fn m1e_session_terminal_has_its_own_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1e.json")).unwrap();
    assert_eq!(
        serde_json::to_value(Capability::session_terminal()).unwrap(),
        fixture["capability"]
    );
    for value in fixture["requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), *value);
        assert_eq!(request.capability(), Some(Capability::session_terminal()));
        // Every terminal request is acknowledged with Ok, never another reply.
        assert!(request.accepts(&Response::Ok));
        assert!(!request.accepts(&Response::SessionScreen { lines: vec![] }));
    }
    let input: Request = serde_json::from_value(fixture["requests"][3].clone()).unwrap();
    assert!(
        matches!(input, Request::TerminalInput { data, paste: false, .. } if data == [0, 255, 10, 13, 27])
    );
    // `paste` defaults to false.
    let typed: Request =
        serde_json::from_value(json!({"cmd":"terminal_input","session":"s","data":"eA=="}))
            .unwrap();
    assert!(matches!(typed, Request::TerminalInput { paste: false, .. }));
    for value in fixture["messages"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
        if let Message::Event { event } = message {
            assert_eq!(event.capability(), Capability::session_terminal());
            assert!(event.into_terminal().is_some_and(|(s, _)| s == "s1"));
        }
    }
    let output: Event = serde_json::from_value(fixture["messages"][3]["event"].clone()).unwrap();
    assert_eq!(
        output.into_terminal(),
        Some((
            "s1".into(),
            TerminalFrame::Output {
                data: b"ls\n".to_vec(),
                echo: Some(EchoTimes {
                    wrote: u64::MAX,
                    read: 0
                })
            }
        ))
    );
    // The removed M0 families do not decode; bad bytes fail.
    assert!(serde_json::from_value::<Event>(json!({"event":"output","session":"s","data":""})).is_err());
    assert!(serde_json::from_value::<Request>(json!({"cmd":"subscribe","session":"s"})).is_err());
    for bad in [
        json!({"event":"terminal_output","session":"s","data":"not base64!"}),
        json!({"event":"terminal_snapshot","session":"s","data":"","cols":-1,"rows":0}),
        json!({"event":"terminal_snapshot","session":"s","data":""}),
    ] {
        assert!(serde_json::from_value::<Event>(bad).is_err());
    }
    assert!(serde_json::from_value::<Request>(
        json!({"cmd":"terminal_resize","session":"s","cols":65536,"rows":1})
    )
    .is_err());
    // Only a local endpoint may be managed (spawn, signal, reload, link).
    assert!(Endpoint::Local {
        socket: "/s".into()
    }
    .is_local());
    assert!(!Endpoint::Ssh {
        hostname: "h".into(),
        account: "a".into(),
        port: 22,
        socket: "/s".into()
    }
    .is_local());
}

#[test]
fn m1g_lifecycle_and_groups_have_their_own_capabilities_and_lossless_golden_shapes() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m1g.json")).unwrap();
    let lifecycle = Capability::session_lifecycle();
    let groups = Capability::session_groups();
    assert_eq!(
        serde_json::to_value(&lifecycle).unwrap(),
        fixture["capabilities"]["lifecycle"]
    );
    assert_eq!(
        serde_json::to_value(&groups).unwrap(),
        fixture["capabilities"]["groups"]
    );
    let row = Response::Session {
        session: Box::new(
            serde_json::from_value(fixture["replies"][0]["result"]["Ok"]["session"].clone())
                .unwrap(),
        ),
    };
    let group: Group =
        serde_json::from_value(fixture["replies"][2]["result"]["Ok"]["group"].clone()).unwrap();
    for (key, capability) in [
        ("lifecycle_requests", &lifecycle),
        ("group_requests", &groups),
    ] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(capability));
            // Each request takes exactly one reply shape; Ok never stands in for a row.
            let wants_row = matches!(
                request,
                Request::SessionCreate { .. }
                    | Request::SessionRename { .. }
                    | Request::SessionSetTheme { .. }
            );
            assert_eq!(request.accepts(&row), wants_row, "{value}");
            assert_eq!(
                request.accepts(&Response::Groups { groups: vec![] }),
                matches!(request, Request::GroupList)
            );
            assert_eq!(
                request.accepts(&Response::Group {
                    group: group.clone()
                }),
                matches!(request, Request::GroupSave { .. })
            );
            assert_eq!(
                request.accepts(&Response::Ok),
                matches!(
                    request,
                    Request::SessionKill { .. }
                        | Request::SessionSeen { .. }
                        | Request::GroupDelete { .. }
                        | Request::GroupOrder { .. }
                )
            );
        }
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Spec defaults match the legacy SessionSpec: shell role, 120×40, no peek.
    let minimal: Request =
        serde_json::from_value(json!({"cmd":"session_create","spec":{}})).unwrap();
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"cmd":"session_create","spec":{"label":"","name":null,"role":"shell","cwd":null,"command":null,"args":[],"peek":false,"cols":120,"rows":40}})
    );
    // A coordinator flag is not part of the contract; unknown fields are ignored.
    let ignored: Request =
        serde_json::from_value(json!({"cmd":"session_create","spec":{"coordinator":true}}))
            .unwrap();
    assert_eq!(ignored, minimal);
    for bad in [
        json!({"cmd":"session_create","spec":{"role":"robot"}}),
        json!({"cmd":"session_create","spec":{"cols":-1}}),
        json!({"cmd":"session_kill"}),
        json!({"cmd":"group_save","group":{"name":"x","layout":{"type":"tab"}}}),
        json!({"cmd":"group_save","group":{"name":"x","layout":{"type":"split","dir":"diagonal","ratio":0.5,"a":{"type":"pane","session":"a"},"b":{"type":"pane","session":"b"}}}}),
    ] {
        assert!(
            serde_json::from_value::<Request>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn m3_agent_list_has_its_own_read_only_capability_and_lossless_golden_shape() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m3.json")).unwrap();
    let agents = Capability::session_agents();
    assert_eq!(
        serde_json::to_value(&agents).unwrap(),
        fixture["capabilities"]["agents"]
    );
    let list = Response::Agents { agents: vec![] };
    for value in fixture["agent_requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), *value);
        assert_eq!(request.capability(), Some(agents.clone()));
        // Exactly one reply shape: never Ok, a row or groups.
        assert!(request.accepts(&list));
        assert!(!request.accepts(&Response::Ok));
        assert!(!request.accepts(&Response::Groups { groups: vec![] }));
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // No other request is answered by an agent list.
    let kill: Request = serde_json::from_value(json!({"cmd":"session_kill","session":"s"})).unwrap();
    assert!(!kill.accepts(&list));
    // The list grants nothing else: lifecycle and groups keep their own.
    assert_ne!(kill.capability(), Some(agents.clone()));
    // A row needs its id, command and states; extra fields are ignored.
    for bad in [
        json!({"kind":"agents","agents":[{"id":"x","command":"x","installed":true}]}),
        json!({"kind":"agents","agents":[{"command":"x","installed":true,"enabled":true}]}),
        json!({"kind":"agents"}),
    ] {
        assert!(
            serde_json::from_value::<Response>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn m4_projects_and_git_status_are_read_only_data_plane_families() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m4.json")).unwrap();
    let projects = Capability::projects();
    let git = Capability::git_status();
    let diff_cap = Capability::git_diff();
    assert_eq!(serde_json::to_value(&projects).unwrap(), fixture["capabilities"]["projects"]);
    assert_eq!(serde_json::to_value(&git).unwrap(), fixture["capabilities"]["git_status"]);
    assert_eq!(serde_json::to_value(&diff_cap).unwrap(), fixture["capabilities"]["git_diff"]);
    let list = Response::Projects { projects: vec![], revision: None };
    let changes = Response::Changes { changes: vec![], base: None };
    let diff = Response::Diff {
        diff: skiff_host_api::HostDiff { text: String::new(), truncated: false, binary: false, untracked: false },
    };
    for (key, cap) in [("project_requests", &projects), ("git_requests", &git), ("diff_requests", &diff_cap)] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(cap));
            // Exactly one reply kind each, never Ok.
            assert!(!request.accepts(&Response::Ok));
            assert_eq!(request.accepts(&changes), cap == &git);
            assert_eq!(request.accepts(&diff), cap == &diff_cap);
        }
    }
    assert!(Request::ProjectList.accepts(&list));
    assert!(!Request::ProjectGet { name: "x".into() }.accepts(&list));
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Neither family grants a session operation, nor the other family.
    for session in [Request::SessionInventory, Request::AgentList, Request::GroupList] {
        let c = session.capability().unwrap();
        assert!(c != projects && c != git && c != diff_cap);
    }
    assert_ne!(projects, git);
    assert_ne!(git, diff_cap);
    // An older client's request without `file` is the whole worktree's.
    let whole: Request = serde_json::from_value(json!({"cmd":"git_diff","worktree":"/w"})).unwrap();
    assert_eq!(whole, Request::GitDiff { worktree: "/w".into(), file: None });
    // A row needs its states; a status is one of four letters.
    for bad in [
        json!({"kind":"changes","changes":[{"path":"a","status":"X","added":1,"removed":0}]}),
        json!({"kind":"changes","changes":[{"path":"a","added":1,"removed":0}]}),
        json!({"kind":"projects","projects":[{"name":"x","short":"x","path":"/x","color":null,"agents":[],"worktrees":[],"error":null}]}),
        json!({"kind":"project","project":{"name":"x"}}),
        json!({"kind":"diff","diff":{"text":"","truncated":false,"binary":false}}),
    ] {
        let ok = serde_json::from_value::<Response>(bad.clone()).is_ok();
        // A project with only the required fields decodes: the rest default.
        assert_eq!(ok, bad["kind"] == "projects", "{bad}");
    }
}

#[test]
fn m4_project_and_worktree_changes_have_their_own_capabilities_and_golden_shapes() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m4b.json")).unwrap();
    let config = Capability::project_config();
    let worktrees = Capability::worktrees();
    assert_eq!(serde_json::to_value(&config).unwrap(), fixture["capabilities"]["project_config"]);
    assert_eq!(serde_json::to_value(&worktrees).unwrap(), fixture["capabilities"]["worktrees"]);
    let revision = Response::Revision { revision: "r".into() };
    let branches = Response::Branches { branches: skiff_host_api::HostBranches { head: None, branches: vec![] } };
    for (key, cap) in [("project_requests", &config), ("worktree_requests", &worktrees)] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(cap));
            // Reading never grants a change, and neither change family the other.
            assert!(cap != &Capability::projects() && cap != &Capability::git_status());
            let edits = matches!(request, Request::ProjectEdit { .. } | Request::ProjectArrange { .. } | Request::ProjectRemove { .. });
            assert_eq!(request.accepts(&revision), edits, "{value}");
            // Only a worktree removal answers `ok`.
            assert_eq!(request.accepts(&Response::Ok), matches!(request, Request::WorktreeRemove { .. }), "{value}");
            assert_eq!(request.accepts(&branches), matches!(request, Request::BranchList { .. }), "{value}");
        }
    }
    assert_ne!(config, worktrees);
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // A Host that predates the revision still answers a list an older client reads.
    let old: Response = serde_json::from_value(json!({"kind":"projects","projects":[]})).unwrap();
    assert_eq!(old, Response::Projects { projects: vec![], revision: None });
    assert_eq!(serde_json::to_value(&old).unwrap(), json!({"kind":"projects","projects":[]}));
    // Every change names its revision: one without it is malformed.
    for bad in [
        json!({"cmd":"project_edit","project":"web","edit":{"key":"files","on":true}}),
        json!({"cmd":"project_remove","project":"web"}),
        json!({"cmd":"project_arrange","order":[]}),
        json!({"cmd":"project_add","path":"/srv/new"}),
        json!({"cmd":"project_edit","project":"web","revision":"r","edit":{"key":"icon","icon":"/etc/passwd"}}),
        json!({"cmd":"branch_list"}),
    ] {
        assert!(serde_json::from_value::<Request>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn m5_files_and_assets_are_read_only_families_of_their_own() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m5.json")).unwrap();
    let files = Capability::files();
    let assets = Capability::project_assets();
    assert_eq!(serde_json::to_value(&files).unwrap(), fixture["capabilities"]["files"]);
    assert_eq!(serde_json::to_value(&assets).unwrap(), fixture["capabilities"]["project_assets"]);
    let listing = Response::Files { listing: skiff_host_api::HostListing { entries: vec![], truncated: false } };
    let file = Response::File {
        file: Box::new(skiff_host_api::HostFile {
            content: skiff_host_api::FileContent::Binary,
            size: 0,
            mime: None,
            text: None,
            bytes: None,
            truncated: false,
            target: None,
        }),
    };
    let image = Response::Image { image: skiff_host_api::HostImage { mime: "image/png".into(), bytes: vec![] } };
    for (key, cap) in [("file_requests", &files), ("asset_requests", &assets)] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(cap));
            // Exactly one reply kind each, never Ok.
            assert!(!request.accepts(&Response::Ok));
            assert_eq!(request.accepts(&listing), matches!(request, Request::FileList { .. }), "{value}");
            assert_eq!(request.accepts(&file), matches!(request, Request::FileRead { .. }), "{value}");
            assert_eq!(request.accepts(&image), matches!(request, Request::ProjectBackground { .. }), "{value}");
        }
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Neither family is granted by another, nor grants a write or Git.
    for other in [Capability::projects(), Capability::git_status(), Capability::git_diff(), Capability::project_config(), Capability::worktrees()] {
        assert!(other != files && other != assets);
    }
    assert_ne!(files, assets);
    // Without `dir` the worktree itself; without `purpose`, a preview.
    let top: Request = serde_json::from_value(json!({"cmd":"file_list","worktree":"/w"})).unwrap();
    assert_eq!(top, Request::FileList { worktree: "/w".into(), dir: String::new(), follow_links: false });
    let read: Request = serde_json::from_value(json!({"cmd":"file_read","worktree":"/w","file":"a"})).unwrap();
    assert_eq!(read, Request::FileRead { worktree: "/w".into(), file: "a".into(), purpose: skiff_host_api::ReadPurpose::Preview });
    for bad in [
        json!({"cmd":"file_read","worktree":"/w"}),
        json!({"cmd":"file_read","worktree":"/w","file":"a","purpose":"write"}),
        json!({"cmd":"file_list","dir":"a"}),
        json!({"cmd":"project_background"}),
    ] {
        assert!(serde_json::from_value::<Request>(bad.clone()).is_err(), "{bad}");
    }
    for bad in [
        json!({"kind":"file","file":{"content":"text","size":1}}),
        json!({"kind":"file","file":{"content":"script","size":1,"truncated":false}}),
        json!({"kind":"file","file":{"content":"image","size":1,"truncated":false,"bytes":"not base64!"}}),
        json!({"kind":"files","listing":{"entries":[{"name":"a","kind":"socket"}],"truncated":false}}),
        json!({"kind":"image","image":{"mime":"image/png"}}),
    ] {
        assert!(serde_json::from_value::<Response>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn m6_ledger_and_skipper_are_read_only_families_of_their_own() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m6.json")).unwrap();
    let ledger = Capability::ledger_read();
    let skipper = Capability::skipper_settings();
    assert_eq!(serde_json::to_value(&ledger).unwrap(), fixture["capabilities"]["ledger"]);
    assert_eq!(serde_json::to_value(&skipper).unwrap(), fixture["capabilities"]["skipper"]);
    let rows = Response::Ledger { rows: vec![] };
    let stamp = Response::LedgerStamp { stamp: "s".into() };
    let file = Response::File {
        file: Box::new(skiff_host_api::HostFile {
            content: skiff_host_api::FileContent::Text,
            size: 0,
            mime: None,
            text: None,
            bytes: None,
            truncated: false,
            target: None,
        }),
    };
    let settings: Response = serde_json::from_value(fixture["replies"][4]["result"]["Ok"].clone()).unwrap();
    for (key, cap) in [("ledger_requests", &ledger), ("skipper_requests", &skipper)] {
        for value in fixture[key].as_array().unwrap() {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&request).unwrap(), *value);
            assert_eq!(request.capability().as_ref(), Some(cap));
            // Exactly one reply kind each, never Ok.
            assert!(!request.accepts(&Response::Ok));
            assert_eq!(request.accepts(&rows), matches!(request, Request::LedgerRows), "{value}");
            assert_eq!(request.accepts(&stamp), matches!(request, Request::LedgerStamp), "{value}");
            assert_eq!(request.accepts(&file), matches!(request, Request::LedgerResult { .. }), "{value}");
            assert_eq!(request.accepts(&settings), matches!(request, Request::SkipperSettings), "{value}");
        }
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // A row names its work's bucket; a row from a Host that predates it
    // still reads, with none.
    let Response::Ledger { rows } = serde_json::from_value(fixture["replies"][0]["result"]["Ok"].clone()).unwrap() else { panic!("ledger rows") };
    assert_eq!(rows.iter().map(|r| r.bucket.as_str()).collect::<Vec<_>>(), ["open", ""]);
    // Neither family is granted by another, nor by files: a result is read
    // by its row, never as a worktree file.
    for other in [Capability::projects(), Capability::project_config(), Capability::files(), Capability::project_assets()] {
        assert!(other != ledger && other != skipper);
    }
    assert_ne!(ledger, skipper);
    // A result is named by its row only: a path is not part of the request.
    for bad in [
        json!({"cmd":"ledger_result"}),
        json!({"cmd":"ledger_result","path":"/etc/hosts"}),
        json!({"cmd":"ledger_write","uid":"a1b2c3d4"}),
    ] {
        assert!(serde_json::from_value::<Request>(bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn m5_folders_is_a_family_of_its_own_that_lists_folders_only() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/m5f.json")).unwrap();
    let folders = Capability::folders();
    assert_eq!(serde_json::to_value(&folders).unwrap(), fixture["capabilities"]["folders"]);
    let path = Response::Path { path: "/h".into() };
    let listing = Response::Folders {
        listing: FolderListing { path: "/".into(), real: "/".into(), parent: None, entries: vec![], truncated: false },
    };
    let places = Response::Places { places: FolderPlaces { recent: vec![], repos: vec![], scanning: false } };
    for value in fixture["folder_requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), *value);
        assert_eq!(request.capability().as_ref(), Some(&folders));
        // Exactly one reply kind each, never Ok.
        assert!(!request.accepts(&Response::Ok));
        assert_eq!(request.accepts(&path), matches!(request, Request::FolderHome | Request::FolderCreate { .. }), "{value}");
        assert_eq!(request.accepts(&places), matches!(request, Request::FolderPlaces), "{value}");
        assert_eq!(request.accepts(&listing), matches!(request, Request::FolderList { .. }), "{value}");
    }
    for value in fixture["replies"].as_array().unwrap() {
        let message: Message = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), *value);
    }
    // Not granted by files, which reads inside open worktrees only.
    for other in [Capability::files(), Capability::projects(), Capability::project_config(), Capability::project_assets()] {
        assert_ne!(other, folders);
    }
    // Without `hidden`, hidden folders stay out.
    let plain: Request = serde_json::from_value(json!({"cmd":"folder_list","path":"/h"})).unwrap();
    assert_eq!(plain, Request::FolderList { path: "/h".into(), hidden: false, filter: None });
    // Initialize repository is a project_config change, never a folders one.
    let init: Request = serde_json::from_value(json!({"cmd":"project_init","path":"/h/notes"})).unwrap();
    assert_eq!(init.capability(), Some(Capability::project_config()));
    assert!(init.accepts(&serde_json::from_value(json!({"kind":"folder","folder":{"root":"/h/notes","name":"notes","short":"N","color":"#123456","branch":null,"worktrees":1,"error":null}})).unwrap()));
    assert!(!init.accepts(&Response::Ok));
    for bad in [
        json!({"cmd":"folder_list"}),
        json!({"cmd":"folder_create","parent":"/h"}),
        json!({"cmd":"folder_read","path":"/etc/hosts"}),
    ] {
        assert!(serde_json::from_value::<Request>(bad.clone()).is_err(), "{bad}");
    }
    for bad in [
        json!({"kind":"folders","listing":{"path":"/","real":"/","parent":null,"entries":[{"name":"a","kind":"file"}],"truncated":false}}),
        json!({"kind":"folders","listing":{"path":"/","entries":[],"truncated":false}}),
        json!({"kind":"path"}),
    ] {
        assert!(serde_json::from_value::<Response>(bad.clone()).is_err(), "{bad}");
    }
}

/// Skippers settings: the instruction files are a family of their own,
/// named by what they are for, never by path. A write carries a revision.
/// The two switches Skippers dropped are no edits.
#[test]
fn skipper_files_are_a_family_of_their_own() {
    let cap = Capability::skipper_file();
    assert_eq!(serde_json::to_value(&cap).unwrap(), json!({"name":"skipper_file","version":1}));
    let file = Response::SkipperFile { file: skiff_host_api::HostSkipperFile { text: "# x\n".into(), revision: "r1".into() } };
    let settings: Response = serde_json::from_value(serde_json::from_str::<Value>(include_str!("fixtures/m6.json")).unwrap()["replies"][4]["result"]["Ok"].clone()).unwrap();
    for value in [
        json!({"cmd":"skipper_file_read","scope":{"kind":"skipper"}}),
        json!({"cmd":"skipper_file_read","scope":{"kind":"fleet","name":"Harbor"}}),
        json!({"cmd":"skipper_file_write","scope":{"kind":"project","name":"ops-web"},"text":"mine","revision":"r1"}),
        json!({"cmd":"skipper_file_write","scope":{"kind":"type","name":"review"},"text":"","revision":""}),
        json!({"cmd":"skipper_file_reset","scope":{"kind":"project","name":"ops-web"}}),
    ] {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        assert_eq!(request.capability(), Some(cap.clone()), "{value}");
        assert!(!request.accepts(&Response::Ok), "{value}");
        let reset = matches!(request, Request::SkipperFileReset { .. });
        assert_eq!(request.accepts(&file), !reset, "{value}");
        assert_eq!(request.accepts(&settings), reset, "{value}");
    }
    for bad in [
        json!({"cmd":"skipper_file_read","path":"/etc/passwd"}),
        json!({"cmd":"skipper_file_read","scope":{"kind":"other","name":"x"}}),
        json!({"cmd":"skipper_file_write","scope":{"kind":"skipper"},"text":"x"}),
        json!({"cmd":"skipper_edit","edit":{"key":"enabled","value":true}}),
        json!({"cmd":"skipper_edit","edit":{"key":"project_skippers","value":true}}),
    ] {
        assert!(serde_json::from_value::<Request>(bad.clone()).is_err(), "{bad}");
    }
    for other in [Capability::skipper_settings(), Capability::skipper_config(), Capability::files()] {
        assert!(other != cap);
    }
}

/// #166 R7 and R8: Skipper's changes, its session and watcher, and the
/// agents table are families of their own, each with one reply kind.
#[test]
fn skipper_changes_launch_and_agent_changes_are_families_of_their_own() {
    let cases = [
        (json!({"cmd":"skipper_edit","edit":{"key":"name","value":"Mate"}}), Capability::skipper_config()),
        (json!({"cmd":"skipper_edit","edit":{"key":"add_task","value":{"type":"review","agent":"claude","model":null}}}), Capability::skipper_config()),
        (json!({"cmd":"skipper_edit","edit":{"key":"model","value":"opus"}}), Capability::skipper_config()),
        (json!({"cmd":"skipper_edit","edit":{"key":"project_model","value":null}}), Capability::skipper_config()),
        (json!({"cmd":"skipper_folder_preview","path":"~/skiff-agent"}), Capability::skipper_config()),
        (json!({"cmd":"skipper_folder_change","path":"/home/me/a","mode":"copy","end_skipper":true,"anyway":false}), Capability::skipper_config()),
        (json!({"cmd":"skipper_open"}), Capability::skipper_run()),
        (json!({"cmd":"skipper_open","restore":4}), Capability::skipper_run()),
        (json!({"cmd":"skipper_watch","session":"s1"}), Capability::skipper_run()),
        (json!({"cmd":"agent_config"}), Capability::agent_config()),
        (json!({"cmd":"agent_edit","revision":"r1","edit":{"key":"enabled","agents":["claude"]}}), Capability::agent_config()),
        (json!({"cmd":"agent_edit","revision":"r1","edit":{"key":"restore"}}), Capability::agent_config()),
    ];
    for (value, cap) in cases {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        assert_eq!(request.capability(), Some(cap));
        // Only an open answers `ok`: a restore that found no session.
        assert_eq!(request.accepts(&Response::Ok), matches!(request, Request::SkipperOpen { .. }), "{value}");
    }
    let agents = Response::AgentConfig { agents: vec![], revision: "r2".into() };
    let edit: Request = serde_json::from_value(json!({"cmd":"agent_edit","revision":"r1","edit":{"key":"restore"}})).unwrap();
    assert!(edit.accepts(&agents));
    assert!(!Request::SkipperWatch { session: "s1".into() }.accepts(&agents));
    assert!(Request::SkipperWatch { session: "s1".into() }.accepts(&Response::Watch { started: true }));
}

#[test]
fn fleet_skippers_are_skipper_run_major_2_apart_from_major_1() {
    let fleets = Capability::skipper_run_fleets();
    assert_eq!((fleets.name.as_str(), fleets.version), ("skipper_run", 2));
    assert_ne!(fleets, Capability::skipper_run());
    let taken = Response::FleetTakeover {
        taken: vec![skiff_host_api::TakenSkipper {
            project: "web".into(),
            session: "s2".into(),
            tasks: vec![skiff_host_api::TakenTask { uid: "c0d4e7f2".into(), title: "Update client".into() }],
            handoffs: vec!["9f1d0c3e".into()],
        }],
    };
    let cases = [
        json!({"cmd":"skipper_fleet","fleet":"Harbor"}),
        json!({"cmd":"fleet_takeover_preview","fleet":"Harbor"}),
        json!({"cmd":"fleet_takeover_preview","fleet":"Harbor","join":"web"}),
        json!({"cmd":"fleet_takeover","fleet":"Harbor","join":"web"}),
        json!({"cmd":"fleet_rename","old":"Harbor","new":"Port"}),
    ];
    for value in cases {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        assert_eq!(request.capability(), Some(Capability::skipper_run_fleets()), "{value}");
        let takeover = matches!(request, Request::FleetTakeoverPreview { .. } | Request::FleetTakeover { .. });
        assert_eq!(request.accepts(&taken), takeover, "{value}");
        assert_eq!(request.accepts(&Response::Ok), matches!(request, Request::FleetRename { .. }), "{value}");
    }
    // A retiring session keeps the field across the wire; a plain one omits it.
    let mut plain: skiff_host_api::InventorySession = serde_json::from_value(json!({
        "id":"s1","label":"claude","role":"agent","cwd":"/w","command":"claude","args":[],"state":"idle","cols":80,"rows":24,"pid":null,"exit_code":null
    }))
    .unwrap();
    assert!(serde_json::to_value(&plain).unwrap().get("retiring").is_none());
    plain.retiring = true;
    assert_eq!(serde_json::to_value(&plain).unwrap()["retiring"], json!(true));
}
