mod common;

use common::ThoughtJackProcess;
use serde_json::json;

// ============================================================================
// resource-exfiltration: serves resources with injected credentials
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn resource_exfiltration_serves_resources() {
    let mut proc = ThoughtJackProcess::spawn_scenario("resource-exfiltration");

    let init_resp = proc.send_initialize().await;

    // Verify resources capability is advertised
    let caps = init_resp
        .pointer("/result/capabilities")
        .expect("should have capabilities");
    assert!(
        caps.get("resources").is_some(),
        "should advertise resources capability"
    );

    // List resources
    let list_resp = proc.send_request("resources/list", None).await;
    let resources = list_resp
        .pointer("/result/resources")
        .and_then(serde_json::Value::as_array)
        .expect("resources/list should return an array");

    assert!(
        !resources.is_empty(),
        "resource-exfiltration should serve at least one resource"
    );

    // Verify resource entries have required fields
    for resource in resources {
        assert!(
            resource.get("uri").is_some(),
            "resource should have uri: {resource:?}"
        );
        assert!(
            resource.get("name").is_some(),
            "resource should have name: {resource:?}"
        );
    }

    // Read the first resource
    let first_uri = resources[0]
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .expect("resource should have uri string");

    let read_resp = proc
        .send_request("resources/read", Some(json!({"uri": first_uri})))
        .await;

    let contents = read_resp
        .pointer("/result/contents")
        .and_then(serde_json::Value::as_array)
        .expect("resources/read should return contents array");

    assert!(
        !contents.is_empty(),
        "resources/read should return at least one content block"
    );

    // Verify content has text
    let text = contents[0].get("text").and_then(serde_json::Value::as_str);
    assert!(
        text.is_some(),
        "resource content should have text: {contents:?}"
    );
    assert!(
        !text.unwrap().is_empty(),
        "resource content text should be non-empty"
    );

    proc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_exfiltration_reads_multiple_resources() {
    let mut proc = ThoughtJackProcess::spawn_scenario("resource-exfiltration");

    proc.send_initialize().await;

    let list_resp = proc.send_request("resources/list", None).await;
    let resources = list_resp
        .pointer("/result/resources")
        .and_then(serde_json::Value::as_array)
        .expect("resources/list should return an array");

    // Read each resource and verify it returns content
    for resource in resources {
        let uri = resource
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .expect("resource should have uri");

        let read_resp = proc
            .send_request("resources/read", Some(json!({"uri": uri})))
            .await;

        assert!(
            read_resp.get("result").is_some(),
            "resources/read for {uri} should succeed: {read_resp:?}"
        );
    }

    proc.shutdown().await;
}
