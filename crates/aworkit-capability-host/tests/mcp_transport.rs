use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use aworkit_capability_host::{
    InjectionTargetV1, MCP_PROTOCOL_2025_11_25, MCP_PROTOCOL_2026_07_28, McpCallKindV1, McpCallV1,
    McpPeerTransportConfigV1, McpServerManifestV1, McpSessionManager, McpStdioTransportConfigV1,
    McpStreamableHttpTransportConfigV1, McpTransportEndpointV1, McpTransportKindV1,
    ProductionMcpPeer, RedeemLeaseRequestV1, SecretDeliveryV1, SecretFieldPlanV1,
    SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializer,
};
use aworkit_protocol::{ProcessGeneration, StableId};
use zeroize::Zeroizing;

const BINDING_HASH: &str =
    "sha256:5555555555555555555555555555555555555555555555555555555555555555";
const HTTP_SECRET: &str = "mcp-fixture-token";

fn stable_id(value: &str) -> StableId {
    StableId::parse(value).expect("test stable ID")
}

fn python_executable() -> PathBuf {
    ["/usr/bin/python3", "/usr/local/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .expect("a system Python 3 interpreter for the hermetic MCP fixture")
}

fn fixture_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_fixture.py")
}

fn manifest(
    server_id: &str,
    transport: McpTransportKindV1,
    minimum_protocol_version: u16,
) -> McpServerManifestV1 {
    McpServerManifestV1 {
        server_id: stable_id(server_id),
        adapter_version: "rmcp-3.1.4".into(),
        binding_hash: BINDING_HASH.into(),
        host_generation: ProcessGeneration(17),
        configured: true,
        enabled: true,
        core_attested: true,
        transport,
        minimum_protocol_version,
        maximum_protocol_version: MCP_PROTOCOL_2026_07_28,
        maximum_in_flight: 2,
        maximum_progress_events: 4,
        secret_slots: Vec::new(),
        workspace_roots: Vec::new(),
    }
}

#[test]
fn real_stdio_peer_negotiates_legacy_discovers_and_calls_a_tool() {
    let config = McpPeerTransportConfigV1 {
        server_id: stable_id("mcp.fixture"),
        binding_hash: BINDING_HASH.into(),
        endpoint: McpTransportEndpointV1::Stdio(McpStdioTransportConfigV1 {
            executable: python_executable(),
            arguments: vec![fixture_script().display().to_string()],
            working_directory: None,
            public_environment: BTreeMap::new(),
        }),
    };
    let peer = Arc::new(ProductionMcpPeer::new(vec![config]).expect("production MCP peer"));
    let manager = McpSessionManager::new(ProcessGeneration(17), peer);

    let snapshot = manager
        .open(manifest(
            "mcp.fixture",
            McpTransportKindV1::Stdio,
            MCP_PROTOCOL_2025_11_25,
        ))
        .expect("real STDIO initialization");
    assert_eq!(snapshot.protocol_version, MCP_PROTOCOL_2025_11_25);
    assert!(snapshot.features.tools);
    assert_eq!(snapshot.catalog.tools.len(), 1);
    assert_eq!(snapshot.catalog.tools[0].name, "echo");

    let outcome = manager
        .invoke(
            &stable_id("mcp.fixture"),
            &McpCallV1 {
                invocation_id: stable_id("invocation.fixture-echo"),
                kind: McpCallKindV1::Tool,
                name: "echo".into(),
                expected_schema_hash: Some(snapshot.catalog.tools[0].input_schema_hash.clone()),
                arguments: serde_json::json!({"message": "hello from Aworkit"}),
            },
        )
        .expect("real STDIO tool call");

    assert_eq!(
        outcome
            .result
            .as_ref()
            .and_then(|result| result.get("structuredContent"))
            .and_then(|value| value.get("echo"))
            .and_then(serde_json::Value::as_str),
        Some("hello from Aworkit")
    );
    assert_eq!(outcome.progress.len(), 1);
    assert_eq!(outcome.progress[0].message, "echoed");
    manager.close(&stable_id("mcp.fixture")).expect("close");
}

#[test]
fn real_streamable_http_peer_discovers_and_calls_a_stateless_modern_server() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let server = thread::spawn(move || {
        let mut methods = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("fixture request");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("fixture timeout");
            let (request, headers) = read_http_json(&mut stream);
            assert_eq!(
                headers.get("authorization").map(String::as_str),
                Some("Bearer mcp-fixture-token")
            );
            let method = request["method"]
                .as_str()
                .expect("JSON-RPC method")
                .to_owned();
            let id = request["id"].clone();
            let result = match method.as_str() {
                "server/discover" => serde_json::json!({
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {}},
                    "ttlMs": 0,
                    "cacheScope": "private"
                }),
                "tools/list" => serde_json::json!({
                    "resultType": "complete",
                    "tools": [{
                        "name": "echo",
                        "description": "Echo one message",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"message": {"type": "string"}},
                            "required": ["message"]
                        }
                    }],
                    "ttlMs": 0,
                    "cacheScope": "private"
                }),
                "tools/call" => serde_json::json!({
                    "resultType": "complete",
                    "content": [{"type": "text", "text": "HTTP echo complete"}],
                    "structuredContent": {
                        "echo": request["params"]["arguments"]["message"].clone(),
                        "secretEcho": HTTP_SECRET
                    },
                    "isError": false
                }),
                other => panic!("unexpected fixture method: {other}"),
            };
            methods.push(method);
            write_http_json(
                &mut stream,
                &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
        methods
    });

    let config = McpPeerTransportConfigV1 {
        server_id: stable_id("mcp.http-fixture"),
        binding_hash: BINDING_HASH.into(),
        endpoint: McpTransportEndpointV1::StreamableHttp(McpStreamableHttpTransportConfigV1 {
            endpoint: format!("http://{address}/mcp"),
            allow_stateless: true,
            maximum_sse_event_bytes: 1024 * 1024,
            bearer_token_secret_slot: Some("token".into()),
        }),
    };
    let peer = Arc::new(ProductionMcpPeer::new(vec![config]).expect("production MCP peer"));
    let materialization = SecretMaterializer::new(FixtureLeaseClient)
        .materialize(&SecretMaterializationPlanV1 {
            decision_id: stable_id("decision.http-fixture"),
            invocation_id: stable_id("invocation.http-connect"),
            host_generation: ProcessGeneration(17),
            lease: SecretLeaseHandleV1 {
                lease_id: stable_id("lease.http-fixture"),
            },
            fields: vec![SecretFieldPlanV1 {
                field: "token".into(),
                target: InjectionTargetV1::Header("Authorization".into()),
            }],
        })
        .expect("materialized HTTP credential");
    peer.stage_materialized_secrets(&stable_id("mcp.http-fixture"), materialization)
        .expect("stage HTTP credential");
    let manager = McpSessionManager::new(ProcessGeneration(17), peer);

    let mut http_manifest = manifest(
        "mcp.http-fixture",
        McpTransportKindV1::StreamableHttp,
        MCP_PROTOCOL_2026_07_28,
    );
    http_manifest.secret_slots = vec!["token".into()];
    let snapshot = manager.open(http_manifest).expect("modern HTTP discovery");
    assert_eq!(snapshot.protocol_version, MCP_PROTOCOL_2026_07_28);
    assert_eq!(snapshot.catalog.tools.len(), 1);

    let outcome = manager
        .invoke(
            &stable_id("mcp.http-fixture"),
            &McpCallV1 {
                invocation_id: stable_id("invocation.http-echo"),
                kind: McpCallKindV1::Tool,
                name: "echo".into(),
                expected_schema_hash: Some(snapshot.catalog.tools[0].input_schema_hash.clone()),
                arguments: serde_json::json!({"message": "hello over HTTP"}),
            },
        )
        .expect("modern HTTP tool call");
    assert_eq!(
        outcome
            .result
            .as_ref()
            .and_then(|result| result.get("structuredContent"))
            .and_then(|value| value.get("echo"))
            .and_then(serde_json::Value::as_str),
        Some("hello over HTTP")
    );
    assert_eq!(
        outcome
            .result
            .as_ref()
            .and_then(|result| result.get("structuredContent"))
            .and_then(|value| value.get("secretEcho"))
            .and_then(serde_json::Value::as_str),
        Some("[REDACTED]")
    );
    manager
        .close(&stable_id("mcp.http-fixture"))
        .expect("close");
    assert_eq!(
        server.join().expect("fixture server"),
        ["server/discover", "tools/list", "tools/call"]
    );
}

fn read_http_json(stream: &mut TcpStream) -> (serde_json::Value, BTreeMap<String, String>) {
    let mut received = Vec::new();
    let header_end = loop {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).expect("read fixture request");
        assert!(count > 0, "HTTP request ended before its headers");
        received.extend_from_slice(&buffer[..count]);
        if let Some(index) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        assert!(received.len() <= 64 * 1024, "fixture headers are bounded");
    };
    let headers = std::str::from_utf8(&received[..header_end]).expect("ASCII fixture headers");
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .expect("content-length header");
    let headers = headers
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    while received.len() < header_end + content_length {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).expect("read fixture body");
        assert!(count > 0, "HTTP request ended before its body");
        received.extend_from_slice(&buffer[..count]);
    }
    let body = serde_json::from_slice(&received[header_end..header_end + content_length])
        .expect("JSON fixture request");
    (body, headers)
}

fn write_http_json(stream: &mut TcpStream, body: &serde_json::Value) {
    let body = serde_json::to_vec(body).expect("JSON fixture response");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write fixture headers");
    stream.write_all(&body).expect("write fixture body");
    stream.flush().expect("flush fixture response");
}

#[derive(Clone, Copy)]
struct FixtureLeaseClient;

impl SecretLeaseClientV1 for FixtureLeaseClient {
    fn redeem(
        &self,
        _request: &RedeemLeaseRequestV1,
    ) -> Result<SecretDeliveryV1, SecretMaterializationError> {
        Ok(SecretDeliveryV1 {
            fields: BTreeMap::from([(
                "token".into(),
                Zeroizing::new(HTTP_SECRET.as_bytes().to_vec()),
            )]),
        })
    }

    fn revoke(&self, _lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        Ok(())
    }
}
