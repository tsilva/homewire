use homewire::{
    Result,
    mcp::{MAX_MESSAGE, handle, read_message},
};
use serde_json::json;
use std::io::Cursor;

#[test]
fn awair_is_read_only_and_only_available_when_configured() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: true,
        bulbs: false,
        plugs: false,
        light_names: false,
        allow_control: true,
    };
    let list = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    let list = list["result"]["tools"].as_array().expect("tools");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "awair_status");
    assert_eq!(list[0]["annotations"]["readOnlyHint"], true);
    let disabled = ToolSet {
        awair: false,
        bulbs: false,
        plugs: false,
        light_names: false,
        ..tools
    };
    let unknown = handle_configured(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"awair_status"}}),
        disabled,
        |_, _| panic!(),
    )
    .expect("response");
    assert_eq!(unknown["error"]["code"], -32602);
    let args=handle_configured(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"awair_status","arguments":{"host":"192.0.2.1"}}}),tools,|_,_|panic!()).expect("response");
    assert_eq!(args["error"]["code"], -32602);
    let failed = handle_configured(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"awair_status"}}),
        tools,
        |_, _| Err(homewire::error("Awair reading is stale")),
    )
    .expect("response");
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"].get("structuredContent").is_none());
}

#[test]
fn all_device_families_share_one_tool_list() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: true,
        printers: true,
        awair: true,
        bulbs: true,
        plugs: true,
        light_names: false,
        allow_control: false,
    };
    let response = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    let list = response["result"]["tools"].as_array().expect("tools");
    assert_eq!(list.len(), 12);
    assert!(
        list.iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true)
    );
    for name in [
        "awair_status",
        "meaco_status",
        "printer_status",
        "bulb_status",
        "plug_status",
    ] {
        assert!(list.iter().any(|t| t["name"] == name));
    }
}

#[test]
fn bulb_controls_are_advertised_and_dispatched_only_with_control_enabled() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: false,
        bulbs: true,
        plugs: false,
        light_names: false,
        allow_control: false,
    };
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bulb_apply","arguments":{"bulb":"bulb_1","settings":{"power":false}}}});
    let disabled = handle_configured(request.clone(), tools, |_, _| panic!("control disabled"))
        .expect("response");
    assert_eq!(disabled["result"]["isError"], true);
    for control in [false, true] {
        let enabled = ToolSet {
            allow_control: control,
            ..tools
        };
        let response = handle_configured(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            enabled,
            |_, _| panic!(),
        )
        .expect("response");
        let list = response["result"]["tools"].as_array().expect("tools");
        assert_eq!(list.len(), if control { 4 } else { 3 });
        assert_eq!(list.iter().any(|t| t["name"] == "bulb_apply"), control);
    }
    let result = handle_configured(
        request,
        ToolSet {
            allow_control: true,
            ..tools
        },
        |name, args| {
            assert_eq!(name, "bulb_apply");
            assert_eq!(args["bulb"], "bulb_1");
            assert_eq!(args["settings"]["power"], false);
            Ok(json!({"applied":true,"status":{"power":false}}))
        },
    )
    .expect("response");
    assert_eq!(
        result["result"]["structuredContent"]["status"]["power"],
        false
    );
}

#[test]
fn bulb_arguments_are_validated_before_device_dispatch() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: false,
        bulbs: true,
        plugs: false,
        light_names: false,
        allow_control: true,
    };
    for args in [
        json!({}),
        json!({"bulb":42,"settings":{"power":true}}),
        json!({"bulb":"bulb_1","settings":{}}),
        json!({"bulb":"bulb_1","settings":{"hue":20}}),
        json!({"bulb":"bulb_1","settings":{"toggle":true}}),
        json!({"bulb":"bulb_1","settings":{"brightness_percent":0}}),
        json!({"bulb":"bulb_1","settings":{"power":false},"host":"192.0.2.1"}),
    ] {
        let response=handle_configured(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bulb_apply","arguments":args}}),tools,|_,_|panic!()).expect("response");
        assert_eq!(response["error"]["code"], -32602);
    }
    for name in ["bulb_list", "bulb_check"] {
        let response=handle_configured(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":{"bulb":"bulb_1"}}}),tools,|_,_|panic!()).expect("response");
        assert_eq!(response["error"]["code"], -32602);
    }
    let disabled = ToolSet {
        bulbs: false,
        plugs: false,
        light_names: false,
        ..tools
    };
    let response=handle_configured(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"bulb_status","arguments":{"bulb":"bulb_1"}}}),disabled,|_,_|panic!()).expect("response");
    assert_eq!(response["error"]["code"], -32602);
}

#[test]
fn light_names_advertise_shared_models_and_normalize_arguments_and_results() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: false,
        bulbs: true,
        plugs: false,
        light_names: true,
        allow_control: true,
    };
    let response = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    let list = response["result"]["tools"].as_array().expect("tools");
    assert_eq!(list.len(), 4);
    let apply = list
        .iter()
        .find(|t| t["name"] == "light_apply")
        .expect("apply");
    assert_eq!(
        apply["inputSchema"]["properties"]["settings"]["properties"]["color_temperature_k"]["maximum"],
        9000
    );
    assert_eq!(apply["inputSchema"]["required"][0], "light");
    assert!(apply["inputSchema"]["properties"].get("bulb").is_none());
    let response=handle_configured(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"light_apply","arguments":{"light":"led_strip","settings":{"color_temperature_k":7000}}}}),tools,|name,args| {
        assert_eq!(name,"bulb_apply");assert_eq!(args["bulb"],"led_strip");
        Ok(json!({"applied":true,"status":{"bulb":"led_strip","model":"L900"}}))
    }).expect("response");
    assert_eq!(
        response["result"]["structuredContent"]["status"]["light"],
        "led_strip"
    );
    assert!(
        response["result"]["structuredContent"]["status"]
            .get("bulb")
            .is_none()
    );
}
#[test]
fn light_aliases_preserve_control_gating_and_reject_mixed_argument_names() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: false,
        bulbs: true,
        plugs: false,
        light_names: true,
        allow_control: false,
    };
    for args in [
        json!({"bulb":"led_strip"}),
        json!({"light":"led_strip","bulb":"led_strip"}),
        json!({"light":"led_strip","host":"192.0.2.1"}),
    ] {
        let response=handle_configured(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"light_status","arguments":args}}),tools,|_,_|panic!()).expect("response");
        assert_eq!(response["error"]["code"], -32602);
    }
    let response=handle_configured(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"light_apply","arguments":{"light":"led_strip","settings":{"power":false}}}}),tools,|_,_|panic!("No controls")).expect("response");
    assert_eq!(response["result"]["isError"], true);
}

#[test]
fn printer_only_server_exposes_only_read_only_printer_tools() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: true,
        awair: false,
        bulbs: false,
        plugs: false,
        light_names: false,
        allow_control: false,
    };
    let response = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    let list = response["result"]["tools"].as_array().expect("tools");
    assert_eq!(list.len(), 3);
    assert!(
        list.iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true)
    );
    let disabled = handle_configured(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"meaco_status"}}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    assert_eq!(disabled["error"]["code"], -32602);
}
#[test]
fn printer_arguments_are_validated_before_dispatch() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: true,
        printers: true,
        awair: false,
        bulbs: false,
        plugs: false,
        light_names: false,
        allow_control: false,
    };
    for args in [
        json!({}),
        json!({"printer":42}),
        json!({"printer":"hp","host":"192.0.2.1"}),
    ] {
        let response=handle_configured(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"printer_status","arguments":args}}),tools,|_,_|panic!()).expect("response");
        assert_eq!(response["error"]["code"], -32602);
    }
    let result=handle_configured(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"printer_status","arguments":{"printer":"hp"}}}),tools,|name,args|{assert_eq!(name,"printer_status");assert_eq!(args["printer"],"hp");Ok(json!({"state":"idle"}))}).expect("response");
    assert_eq!(result["result"]["structuredContent"]["state"], "idle");
}

#[test]
fn initializes_and_negotiates_supported_version() {
    let result = handle(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"future"}}),
        false,
        |_, _| panic!("No device calls"),
    )
    .expect("response");
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(result["result"]["serverInfo"]["name"], "homewire");
}
#[test]
fn initialization_notification_produces_no_output() {
    assert!(
        handle(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            false,
            |_, _| panic!()
        )
        .is_none()
    );
}
#[test]
fn malformed_requests_receive_invalid_request_instead_of_being_dropped() {
    for request in [
        json!(null),
        json!([]),
        json!({"method":"ping"}),
        json!({"jsonrpc":"2.0","id":{},"method":"ping"}),
    ] {
        let response = handle(request, false, |_, _| panic!("No device calls"))
            .expect("invalid request response");
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], json!(null));
    }
}
#[test]
fn read_only_list_excludes_control_tools() {
    let result = handle(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        false,
        |_, _| panic!(),
    )
    .expect("response");
    assert_eq!(
        result["result"]["tools"].as_array().expect("tools").len(),
        2
    );
}
#[test]
fn disabled_control_never_reaches_device() {
    let result=handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"meaco_apply","arguments":{"operation":"night"}}}),false,|_,_|panic!("Control disabled")).expect("response");
    assert_eq!(result["result"]["isError"], true);
}
#[test]
fn invalid_arguments_do_not_reach_device() {
    for args in [
        json!({"operation":"laundry"}),
        json!({"operation":"day","unexpected":true}),
        json!(null),
    ] {
        let result=handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"meaco_apply","arguments":args}}),true,|_,_|panic!()).expect("response");
        assert_eq!(result["error"]["code"], -32602);
    }
}
#[test]
fn tool_output_is_both_text_and_structured() {
    let result = handle(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"meaco_status"}}),
        false,
        |_, _| Ok(json!({"power":true})),
    )
    .expect("response");
    assert_eq!(result["result"]["structuredContent"]["power"], true);
    assert_eq!(result["result"]["isError"], false);
}
#[test]
fn bounded_reader_rejects_oversized_messages() {
    assert!(read_message(&mut Cursor::new(vec![b'x'; MAX_MESSAGE + 1])).is_err());
}
#[test]
fn reader_preserves_message_boundaries() -> Result<()> {
    let mut reader = Cursor::new(b"one\ntwo\n");
    assert_eq!(read_message(&mut reader)?, Some("one\n".into()));
    assert_eq!(read_message(&mut reader)?, Some("two\n".into()));
    assert_eq!(read_message(&mut reader)?, None);
    Ok(())
}

#[test]
fn plug_tools_are_gated_validate_arguments_and_only_expose_configured_family() {
    use homewire::mcp::{ToolSet, handle_configured};
    let tools = ToolSet {
        meaco: false,
        printers: false,
        awair: false,
        bulbs: false,
        plugs: true,
        light_names: false,
        allow_control: false,
    };
    let list = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tools,
        |_, _| panic!(),
    )
    .expect("response");
    let list = list["result"]["tools"].as_array().expect("tools");
    assert_eq!(list.len(), 3);
    assert!(
        list.iter()
            .all(|t| t["annotations"]["readOnlyHint"] == true)
    );
    let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"plug_apply","arguments":{"plug":"plug1","power":false}}});
    let denied = handle_configured(request.clone(), tools, |_, _| {
        panic!("disabled control dispatched")
    })
    .expect("response");
    assert_eq!(denied["result"]["isError"], true);
    let enabled = ToolSet {
        allow_control: true,
        ..tools
    };
    let list = handle_configured(
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        enabled,
        |_, _| panic!(),
    )
    .expect("response");
    assert_eq!(list["result"]["tools"].as_array().expect("tools").len(), 4);
    let result = handle_configured(request, enabled, |name, args| {
        assert_eq!(name, "plug_apply");
        assert_eq!(args["power"], false);
        Ok(json!({"applied":true}))
    })
    .expect("response");
    assert_eq!(result["result"]["structuredContent"]["applied"], true);
    for args in [
        json!({}),
        json!({"plug":"plug1","power":"off"}),
        json!({"plug":"plug1","power":0}),
        json!({"plug":"plug1","toggle":true}),
        json!({"plug":"plug1","power":true,"host":"192.0.2.1"}),
        json!({"plug":"","power":false}),
    ] {
        let result=handle_configured(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"plug_apply","arguments":args}}),enabled,|_,_|panic!("invalid arguments dispatched")).expect("response");
        assert_eq!(result["error"]["code"], -32602);
    }
    let disabled = ToolSet {
        plugs: false,
        ..enabled
    };
    let result=handle_configured(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"plug_status","arguments":{"plug":"plug1"}}}),disabled,|_,_|panic!()).expect("response");
    assert_eq!(result["error"]["code"], -32602);
    for (name, args) in [
        ("plug_list", json!({"plug":"plug1"})),
        ("plug_check", json!({"host":"192.0.2.1"})),
        ("plug_status", json!({"plug":"plug1","power":true})),
    ] {
        let result=handle_configured(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":name,"arguments":args}}),tools,|_,_|panic!()).expect("response");
        assert_eq!(result["error"]["code"], -32602);
    }
}
