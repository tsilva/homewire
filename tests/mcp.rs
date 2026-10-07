use homewire::{
    Result,
    mcp::{MAX_MESSAGE, handle, read_message},
};
use serde_json::json;
use std::io::Cursor;

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
