//! The MCP server: JSON-RPC 2.0 messages in, answers out, independent of
//! the transport. [`Server::handle`] takes one message and returns the
//! answer, or `None` for a notification. [`Server::notifications`] gives
//! the messages the server sends on its own, like
//! `notifications/tools/list_changed`.
//!
//! The agent sees one front door for the whole mesh. Tools follow the
//! search pattern, so the list stays small and fixed as nodes come and go:
//!
//! - `list_nodes`: this node and its peers, online or not.
//! - `search_capabilities`: methods on online nodes, with their schemas.
//! - `call_capability`: runs one method on one node.
//!
//! Next to them, one direct tool per method on each online node, named
//! `<node>_<capability>_<method>` (MCP clients reject dots in tool names).
//! When a node joins or leaves, or what it offers changes, the direct
//! tools change and the server sends `notifications/tools/list_changed`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::catalog::{Catalog, Method, matches};
use crate::policy::Policy;

/// Protocol versions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What the agent may write for the node the gateway runs on.
pub const LOCAL: &str = "local";

/// Longest tool name MCP clients accept.
const MAX_TOOL_NAME: usize = 64;

const INSTRUCTIONS: &str = "This server controls bricolo nodes: \
personal computers running the bricolo automation node, joined in a mesh. Call \
list_nodes to see which nodes are online, search_capabilities to find what \
they can do (each result names its node and carries the JSON Schema of its \
arguments), then call_capability to run one on a node. Ask the user before \
running a method marked destructive.";

/// Why a call failed, as the node reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct CallFailure {
    /// `unavailable`, `timeout`, `denied` or `failed`.
    pub kind: String,
    pub message: String,
}

/// One node as the backend sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeInfo {
    pub name: String,
    pub online: bool,
    /// True for the node the gateway runs on.
    pub local: bool,
    /// What the node lets this gateway call, as `(capability, method)`
    /// with Rust method names.
    pub methods: Vec<(String, String)>,
    /// Anything else worth showing in `list_nodes`.
    pub details: Value,
}

/// What the gateway calls into: the node it runs on and its peers.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Every node, the local one first.
    async fn nodes(&self) -> Vec<NodeInfo>;

    /// Runs `capability.method` (Rust method name) on `node` with
    /// positional JSON arguments.
    async fn call(
        &self,
        node: &str,
        capability: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, CallFailure>;
}

pub struct Server {
    catalog: Catalog,
    backend: Arc<dyn Backend>,
    policy: Policy,
    version: String,
    notices: broadcast::Sender<Value>,
}

/// A JSON-RPC error.
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn invalid_params(message: impl Into<String>) -> Self {
        RpcError {
            code: -32602,
            message: message.into(),
        }
    }
}

/// A node and the methods the policy lets the agent use on it.
struct View<'a> {
    node: NodeInfo,
    methods: Vec<&'a Method>,
}

impl Server {
    pub fn new(catalog: Catalog, backend: Arc<dyn Backend>, policy: Policy, version: &str) -> Self {
        Server {
            catalog,
            backend,
            policy,
            version: version.to_string(),
            notices: broadcast::channel(16).0,
        }
    }

    /// Messages the server sends without being asked.
    pub fn notifications(&self) -> broadcast::Receiver<Value> {
        self.notices.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn notices_for_test(&self, note: Value) {
        let _ = self.notices.send(note);
    }

    /// Checks the tool list every `every` and sends
    /// `notifications/tools/list_changed` when it differs from the last
    /// check. Stops when the server is dropped.
    pub fn watch(self: &Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut last: Option<BTreeSet<String>> = None;
            loop {
                let Some(server) = weak.upgrade() else { return };
                let now: BTreeSet<String> = server
                    .tools()
                    .await
                    .iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect();
                if last.as_ref().is_some_and(|l| *l != now) {
                    tracing::info!(tools = now.len(), "MCP tool list changed");
                    let _ = server.notices.send(json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed",
                    }));
                }
                last = Some(now);
                drop(server);
                tokio::time::sleep(every).await;
            }
        })
    }

    /// Answers one JSON-RPC message. Notifications and responses get no
    /// answer.
    pub async fn handle(&self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a request we never send, or garbage.
            return id.map(|id| error(id, -32600, "not a JSON-RPC request"));
        };
        let id = id?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tools().await })),
            "tools/call" => self.call_tool(&params).await,
            other => Err(RpcError {
                code: -32601,
                message: format!("no method {other}"),
            }),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => error(id, e.code, &e.message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params["protocolVersion"].as_str().unwrap_or_default();
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == asked)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "bricolo", "version": self.version },
            "instructions": INSTRUCTIONS,
        })
    }

    async fn views(&self) -> Vec<View<'_>> {
        self.backend
            .nodes()
            .await
            .into_iter()
            .map(|node| {
                let methods = self
                    .catalog
                    .select(&node.methods)
                    .into_iter()
                    .filter(|m| self.policy.allows(&node.name, &m.capability, &m.name))
                    .collect();
                View { node, methods }
            })
            .collect()
    }

    /// The tool list: the three search-pattern tools, then one direct tool
    /// per method on each online node.
    pub async fn tools(&self) -> Vec<Value> {
        let views = self.views().await;
        let online: Vec<&str> = views
            .iter()
            .filter(|v| v.node.online)
            .map(|v| v.node.name.as_str())
            .collect();
        let caps: BTreeSet<&str> = views
            .iter()
            .filter(|v| v.node.online)
            .flat_map(|v| v.methods.iter().map(|m| m.capability.as_str()))
            .collect();
        let caps: Vec<&str> = caps.into_iter().collect();
        let node_doc = format!(
            "The node's name, or \"{LOCAL}\" for the one this gateway runs on. Online now: {}.",
            online.join(", ")
        );
        let mut tools = vec![
            json!({
                "name": "list_nodes",
                "title": "List nodes",
                "description": "The nodes in the mesh, with whether each is online. The one this gateway runs on has local: true.",
                "inputSchema": { "type": "object", "properties": {} },
                "annotations": { "readOnlyHint": true, "openWorldHint": false },
            }),
            json!({
                "name": "search_capabilities",
                "title": "Search capabilities",
                "description": format!(
                    "Find methods on the online nodes by keyword. Each result names its node, capability and method, gives the JSON Schema of its arguments, and says whether it only reads or is destructive. An empty query lists everything. Capabilities online: {}.",
                    caps.join(", ")
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Words that must all appear, like \"clipboard\" or \"speak text\"." },
                        "capability": { "type": "string", "description": "Only this capability." },
                        "node": { "type": "string", "description": format!("Only this node. {node_doc}") },
                    },
                },
                "annotations": { "readOnlyHint": true, "openWorldHint": false },
            }),
            json!({
                "name": "call_capability",
                "title": "Call a capability",
                "description": "Run one method on one node. Get the method's argument schema from search_capabilities first. Ask the user before running a destructive method.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node": { "type": "string", "description": format!("{node_doc} Defaults to {LOCAL}.") },
                        "capability": { "type": "string" },
                        "method": { "type": "string" },
                        "args": { "type": "object", "description": "Arguments by name, as in the method's input_schema." },
                    },
                    "required": ["capability", "method"],
                },
                "annotations": { "destructiveHint": true, "openWorldHint": false },
            }),
        ];
        let mut seen = BTreeSet::new();
        for v in views.iter().filter(|v| v.node.online) {
            for m in &v.methods {
                let tool = direct_tool(&v.node.name, m);
                if seen.insert(tool["name"].as_str().unwrap_or_default().to_string()) {
                    tools.push(tool);
                }
            }
        }
        tools
    }

    async fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| RpcError::invalid_params("tools/call needs a name"))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match name {
            "list_nodes" => {
                let nodes: Vec<Value> = self
                    .views()
                    .await
                    .into_iter()
                    .map(|v| {
                        let mut out = json!({
                            "name": v.node.name,
                            "online": v.node.online,
                            "local": v.node.local,
                            "capabilities": v.methods.iter().map(|m| m.capability.as_str()).collect::<BTreeSet<_>>(),
                        });
                        if let (Some(out), Some(details)) = (out.as_object_mut(), v.node.details.as_object()) {
                            for (k, d) in details {
                                out.entry(k.clone()).or_insert(d.clone());
                            }
                        }
                        out
                    })
                    .collect();
                Ok(structured(json!({ "nodes": nodes })))
            }
            "search_capabilities" => {
                let views = self.views().await;
                let only = match args["node"].as_str() {
                    Some(n) => match resolve(&views, n) {
                        Some(v) => Some(v.node.name.clone()),
                        None => return Ok(tool_error(&unknown_node(n, &views))),
                    },
                    None => None,
                };
                let query = args["query"].as_str().unwrap_or_default();
                let cap = args["capability"].as_str();
                let mut found = Vec::new();
                for v in views.iter().filter(|v| v.node.online) {
                    if only.as_ref().is_some_and(|o| *o != v.node.name) {
                        continue;
                    }
                    for m in &v.methods {
                        if cap.is_none_or(|c| m.capability == c) && matches(m, query) {
                            let mut d = m.describe();
                            d["node"] = json!(v.node.name);
                            found.push(d);
                        }
                    }
                }
                Ok(structured(json!({ "results": found })))
            }
            "call_capability" => {
                let (Some(cap), Some(method)) =
                    (args["capability"].as_str(), args["method"].as_str())
                else {
                    return Err(RpcError::invalid_params(
                        "call_capability needs capability and method",
                    ));
                };
                let node = args["node"].as_str().unwrap_or(LOCAL);
                let views = self.views().await;
                let Some(view) = resolve(&views, node) else {
                    return Ok(tool_error(&unknown_node(node, &views)));
                };
                let wanted = method.replace('_', "-");
                let Some(m) = view
                    .methods
                    .iter()
                    .find(|m| m.capability == cap && m.name == wanted)
                else {
                    return Ok(tool_error(&format!(
                        "node {} offers no method {cap}.{method}; use search_capabilities",
                        view.node.name
                    )));
                };
                Ok(self
                    .run(&view.node, m, args.get("args").unwrap_or(&Value::Null))
                    .await)
            }
            direct => {
                let views = self.views().await;
                let found = views.iter().filter(|v| v.node.online).find_map(|v| {
                    v.methods
                        .iter()
                        .find(|m| tool_name(&v.node.name, m) == direct)
                        .map(|m| (&v.node, *m))
                });
                let Some((node, m)) = found else {
                    return Err(RpcError::invalid_params(format!(
                        "no tool named {direct}; its node may have gone offline"
                    )));
                };
                Ok(self.run(node, m, &args).await)
            }
        }
    }

    async fn run(&self, node: &NodeInfo, m: &Method, args: &Value) -> Value {
        if !node.online {
            return tool_error(&format!("node {} is offline (unavailable)", node.name));
        }
        let positional = match m.positional(args) {
            Ok(p) => p,
            Err(e) => return tool_error(&e),
        };
        let outcome = self
            .backend
            .call(&node.name, &m.capability, &m.rust_name(), positional)
            .await;
        tracing::info!(
            target: "audit",
            client = "mcp",
            node = %node.name,
            capability = %m.capability,
            method = %m.name,
            outcome = match &outcome { Ok(_) => "ok", Err(f) => f.kind.as_str() },
            "capability call"
        );
        match outcome {
            Ok(value) => result_content(value),
            Err(f) => tool_error(&format!("{} ({})", f.message, f.kind)),
        }
    }
}

/// The view for `name`, where `local` names the local node.
fn resolve<'a, 'b>(views: &'a [View<'b>], name: &str) -> Option<&'a View<'b>> {
    views.iter().find(|v| v.node.name == name).or_else(|| {
        (name == LOCAL)
            .then(|| views.iter().find(|v| v.node.local))
            .flatten()
    })
}

fn unknown_node(name: &str, views: &[View]) -> String {
    let names: Vec<&str> = views.iter().map(|v| v.node.name.as_str()).collect();
    format!("no node named {name}; nodes: {}", names.join(", "))
}

/// `<node>_<capability>_<method>` with only letters, digits and `_`, at
/// most 64 characters.
fn tool_name(node: &str, m: &Method) -> String {
    let raw = format!("{node}_{}_{}", m.capability, m.name);
    let mut name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    name.truncate(MAX_TOOL_NAME);
    name
}

fn direct_tool(node: &str, m: &Method) -> Value {
    let mut description = m.docs.trim().to_string();
    if description.is_empty() {
        description = m.capability_docs.trim().to_string();
    }
    let mut tool = json!({
        "name": tool_name(node, m),
        "title": format!("{node}: {}", m.id()),
        "description": format!("{description} (on node {node})").trim_start().to_string(),
        "inputSchema": m.input_schema,
        "annotations": {
            "readOnlyHint": m.read_only(),
            "destructiveHint": m.destructive(),
            "openWorldHint": false,
        },
    });
    if let Some(out) = &m.output_schema {
        // MCP output schemas must be objects; wrap the value.
        tool["outputSchema"] = json!({
            "type": "object",
            "properties": { "result": out },
        });
    }
    tool
}

/// A successful call: text for the model, structured content for clients
/// that read it. Bytes that are UTF-8 come back as text too.
fn result_content(value: Value) -> Value {
    let text = as_text(&value).unwrap_or_else(|| match &value {
        Value::Null => "done".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    });
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "result": value },
    })
}

fn structured(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "structuredContent": value,
    })
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn as_text(value: &Value) -> Option<String> {
    let bytes: Vec<u8> = value
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect::<Option<_>>()?;
    if bytes.is_empty() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    type Call = (String, String, String, Vec<Value>);

    /// Two nodes, laptop (local) and studio. Records calls.
    pub(crate) struct Fake {
        pub calls: Mutex<Vec<Call>>,
        pub studio_online: Mutex<bool>,
    }

    #[async_trait]
    impl Backend for Fake {
        async fn nodes(&self) -> Vec<NodeInfo> {
            vec![
                NodeInfo {
                    name: "laptop".into(),
                    online: true,
                    local: true,
                    methods: pairs(&[
                        ("clipboard", "get"),
                        ("clipboard", "set"),
                        ("disk", "read"),
                        ("disk", "write"),
                        ("tts", "speak"),
                        ("speech", "transcribe"),
                    ]),
                    details: json!({ "id": "abc" }),
                },
                NodeInfo {
                    name: "studio".into(),
                    online: *self.studio_online.lock().unwrap(),
                    local: false,
                    methods: pairs(&[("tts", "speak"), ("tts", "status")]),
                    details: json!({ "via": "tailscale" }),
                },
            ]
        }

        async fn call(
            &self,
            node: &str,
            cap: &str,
            method: &str,
            args: Vec<Value>,
        ) -> Result<Value, CallFailure> {
            self.calls
                .lock()
                .unwrap()
                .push((node.into(), cap.into(), method.into(), args));
            match (cap, method) {
                ("clipboard", "get") => Ok(json!("copied")),
                ("disk", "read") => Ok(json!(b"hi".to_vec())),
                ("tts", _) => Ok(Value::Null),
                _ => Err(CallFailure {
                    kind: "denied".into(),
                    message: "denied".into(),
                }),
            }
        }
    }

    pub(crate) fn server_with(policy: Policy) -> (Arc<Server>, Arc<Fake>) {
        let fake = Arc::new(Fake {
            calls: Mutex::default(),
            studio_online: Mutex::new(true),
        });
        let catalog = Catalog::stdlib().unwrap();
        (
            Arc::new(Server::new(catalog, fake.clone(), policy, "0.1.0")),
            fake,
        )
    }

    pub(crate) fn server() -> (Arc<Server>, Arc<Fake>) {
        server_with(Policy::default())
    }

    async fn rpc(s: &Server, method: &str, params: Value) -> Value {
        s.handle(json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }))
            .await
            .unwrap()
    }

    async fn tool(s: &Server, name: &str, args: Value) -> Value {
        rpc(s, "tools/call", json!({ "name": name, "arguments": args })).await["result"].clone()
    }

    async fn names(s: &Server) -> Vec<String> {
        s.tools()
            .await
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn initialize_negotiates_the_version() {
        let (s, _) = server();
        let r = rpc(&s, "initialize", json!({ "protocolVersion": "2025-06-18" })).await;
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], true);
        let r = rpc(&s, "initialize", json!({ "protocolVersion": "1999-01-01" })).await;
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn notifications_get_no_answer_and_unknown_methods_an_error() {
        let (s, _) = server();
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(s.handle(note).await.is_none());
        assert_eq!(rpc(&s, "nope", Value::Null).await["error"]["code"], -32601);
        assert_eq!(rpc(&s, "ping", Value::Null).await["result"], json!({}));
    }

    #[tokio::test]
    async fn direct_tools_per_node_with_valid_names() {
        let (s, _) = server();
        let names = names(&s).await;
        assert_eq!(
            &names[..3],
            ["list_nodes", "search_capabilities", "call_capability"]
        );
        for n in [
            "laptop_clipboard_get",
            "laptop_disk_write",
            "studio_tts_speak",
            "studio_tts_status",
        ] {
            assert!(names.contains(&n.to_string()), "{n} in {names:?}");
        }
        assert!(!names.contains(&"studio_clipboard_get".to_string()));
        let ok =
            |n: &String| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && n.len() <= 64;
        assert!(names.iter().all(ok), "{names:?}");
    }

    #[tokio::test]
    async fn search_spans_nodes_and_calls_reach_the_named_node() {
        let (s, fake) = server();
        let r = tool(&s, "search_capabilities", json!({ "query": "speak" })).await;
        let nodes: BTreeSet<&str> = r["structuredContent"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["node"].as_str().unwrap())
            .collect();
        assert_eq!(nodes, BTreeSet::from(["laptop", "studio"]));

        let r = tool(
            &s,
            "search_capabilities",
            json!({ "node": "local", "capability": "clipboard" }),
        )
        .await;
        assert_eq!(
            r["structuredContent"]["results"].as_array().unwrap().len(),
            2
        );

        let r = tool(
            &s,
            "call_capability",
            json!({ "capability": "clipboard", "method": "get" }),
        )
        .await;
        assert_eq!(r["content"][0]["text"], "copied");
        let r = tool(
            &s,
            "call_capability",
            json!({ "node": "studio", "capability": "tts", "method": "speak", "args": { "text": "hi" } }),
        )
        .await;
        assert_eq!(r["content"][0]["text"], "done");
        let r = tool(&s, "laptop_disk_read", json!({ "path": "~/a" })).await;
        assert_eq!(r["content"][0]["text"], "hi");
        {
            let calls = fake.calls.lock().unwrap();
            assert_eq!(calls[0].0, "laptop");
            assert_eq!(
                calls[1],
                (
                    "studio".into(),
                    "tts".into(),
                    "speak".into(),
                    vec![json!("hi"), Value::Null]
                )
            );
            assert_eq!(calls[2].0, "laptop");
        }

        let r = tool(&s, "list_nodes", json!({})).await;
        let nodes = &r["structuredContent"]["nodes"];
        assert_eq!(nodes[0]["local"], true);
        assert_eq!(nodes[0]["id"], "abc");
        assert_eq!(nodes[1]["via"], "tailscale");
        assert_eq!(nodes[1]["capabilities"], json!(["tts"]));
    }

    #[tokio::test]
    async fn failures_are_tool_errors() {
        let (s, fake) = server();
        let r = tool(&s, "call_capability", json!({ "capability": "disk", "method": "write", "args": { "path": "~/a", "data": "x" } })).await;
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("denied"));

        let r = tool(
            &s,
            "call_capability",
            json!({ "capability": "window", "method": "focused" }),
        )
        .await;
        assert_eq!(r["isError"], true);
        let r = tool(
            &s,
            "call_capability",
            json!({ "capability": "tts", "method": "speak", "args": {} }),
        )
        .await;
        assert!(r["content"][0]["text"].as_str().unwrap().contains("text"));
        let r = tool(
            &s,
            "call_capability",
            json!({ "node": "phone", "capability": "tts", "method": "speak" }),
        )
        .await;
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("laptop, studio")
        );

        *fake.studio_online.lock().unwrap() = false;
        let r = tool(&s, "call_capability", json!({ "node": "studio", "capability": "tts", "method": "speak", "args": { "text": "x" } })).await;
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("offline")
        );
        let r = rpc(
            &s,
            "tools/call",
            json!({ "name": "studio_tts_speak", "arguments": { "text": "x" } }),
        )
        .await;
        assert_eq!(r["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn the_policy_hides_and_refuses() {
        let (s, _) = server_with(Policy::new(
            vec!["laptop".into(), "*.tts.speak".into()],
            vec!["*.disk.write".into()],
        ));
        let names = names(&s).await;
        assert!(names.contains(&"laptop_disk_read".to_string()));
        assert!(!names.contains(&"laptop_disk_write".to_string()));
        assert!(names.contains(&"studio_tts_speak".to_string()));
        assert!(!names.contains(&"studio_tts_status".to_string()));
        let r = tool(&s, "call_capability", json!({ "capability": "disk", "method": "write", "args": { "path": "a", "data": "b" } })).await;
        assert_eq!(r["isError"], true);
    }

    #[tokio::test]
    async fn a_node_leaving_sends_list_changed() {
        let (s, fake) = server();
        let mut notes = s.notifications();
        let watch = s.watch(Duration::from_millis(20));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(notes.try_recv().is_err(), "no change, no notice");
        *fake.studio_online.lock().unwrap() = false;
        let note = tokio::time::timeout(Duration::from_secs(2), notes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(note["method"], "notifications/tools/list_changed");
        assert!(!names(&s).await.contains(&"studio_tts_speak".to_string()));
        watch.abort();
    }
}
