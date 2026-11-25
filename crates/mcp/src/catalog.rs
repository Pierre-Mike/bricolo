//! The capability catalog: every function in the standard library WIT,
//! with its documentation and a JSON Schema for its arguments.
//!
//! The schemas describe the JSON a call carries through the node's JSON
//! dispatch (`bricolo call`): serde's encoding of the provider trait's argument
//! types. Two choices make them easier for an agent:
//!
//! - Arguments are an object keyed by the WIT parameter names in
//!   snake_case. [`Method::positional`] turns it into the array the node
//!   expects.
//! - `list<u8>` also takes a string, sent as its UTF-8 bytes.

use serde_json::{Map, Value, json};
use wit_parser::{Resolve, SourceMap, Type, TypeDefKind, TypeId};

/// The `bricolo:stdlib` package, embedded so an installed `bricolo` needs no
/// repository. A test checks the list against `wit/`.
pub const STDLIB_WIT: &[(&str, &str)] = &[
    ("types.wit", include_str!("../../../wit/types.wit")),
    ("log.wit", include_str!("../../../wit/log.wit")),
    ("timer.wit", include_str!("../../../wit/timer.wit")),
    ("hotkey.wit", include_str!("../../../wit/hotkey.wit")),
    ("tts.wit", include_str!("../../../wit/tts.wit")),
    ("speech.wit", include_str!("../../../wit/speech.wit")),
    ("disk.wit", include_str!("../../../wit/disk.wit")),
    ("clipboard.wit", include_str!("../../../wit/clipboard.wit")),
    ("kv.wit", include_str!("../../../wit/kv.wit")),
    ("window.wit", include_str!("../../../wit/window.wit")),
    ("app.wit", include_str!("../../../wit/app.wit")),
    ("notify.wit", include_str!("../../../wit/notify.wit")),
    ("shell.wit", include_str!("../../../wit/shell.wit")),
    ("http.wit", include_str!("../../../wit/http.wit")),
    ("ui.wit", include_str!("../../../wit/ui.wit")),
    ("keyboard.wit", include_str!("../../../wit/keyboard.wit")),
    ("secrets.wit", include_str!("../../../wit/secrets.wit")),
    ("mic.wit", include_str!("../../../wit/mic.wit")),
    ("audio.wit", include_str!("../../../wit/audio.wit")),
    ("system.wit", include_str!("../../../wit/system.wit")),
    ("files.wit", include_str!("../../../wit/files.wit")),
    ("mouse.wit", include_str!("../../../wit/mouse.wit")),
    ("screen.wit", include_str!("../../../wit/screen.wit")),
    ("selection.wit", include_str!("../../../wit/selection.wit")),
    ("camera.wit", include_str!("../../../wit/camera.wit")),
    (
        "claude-code.wit",
        include_str!("../../../wit/claude-code.wit"),
    ),
    (
        "accessibility.wit",
        include_str!("../../../wit/accessibility.wit"),
    ),
    ("process.wit", include_str!("../../../wit/process.wit")),
    ("device.wit", include_str!("../../../wit/device.wit")),
    ("scripting.wit", include_str!("../../../wit/scripting.wit")),
    (
        "virtual-screen.wit",
        include_str!("../../../wit/virtual-screen.wit"),
    ),
    ("world.wit", include_str!("../../../wit/world.wit")),
];

/// Method names that only read. Everything else may change something.
const READ_ONLY: &[&str] = &[
    "get",
    "read",
    "list",
    "status",
    "transcribe",
    "transcribe-file",
    "frontmost",
    "focused",
    "running",
    "screens",
    "position",
    "power",
    "idle-seconds",
    "get-text",
    "sounds",
    "snapshot",
    "formats",
    "get-image",
    "get-files",
    "list-apps",
    "list-windows",
    "list-shortcuts",
    "find",
    "wait",
];

/// Methods that only read although their names do not say so.
const READ_ONLY_METHODS: &[&str] = &[
    "device.volume",
    "device.muted",
    "device.brightness",
    "device.wifi",
    "device.dark-mode",
    "device.focus-mode",
];

/// Method names that change or remove what is already there, so an agent
/// should confirm them with the user first.
const DESTRUCTIVE: &[&str] = &[
    "write",
    "set",
    "delete",
    "remove",
    "kill",
    "quit",
    "close",
    "run",
    "exec",
    "set-image",
    "set-files",
    "destroy",
];

/// Methods that act on the machine although their names do not say so: a
/// Claude Code session edits files and runs commands, a script or a
/// program on a virtual screen can do anything the user can, stopping a
/// process can lose its work, and locking, sleeping or turning Wi-Fi off
/// can cut the agent off from this node.
const DESTRUCTIVE_METHODS: &[&str] = &[
    "claude-code.start",
    "claude-code.send",
    "claude-code.stop",
    "process.stop",
    "device.lock",
    "device.sleep",
    "device.set-wifi",
    "scripting.run-applescript",
    "scripting.run-jxa",
    "scripting.run-shortcut",
    "scripting.run-powershell",
    "virtual-screen.launch",
];

/// One function of one capability.
#[derive(Clone, Debug)]
pub struct Method {
    /// The WIT interface name, like `disk`.
    pub capability: String,
    /// The WIT function name, like `read` or `transcribe-file`.
    pub name: String,
    pub docs: String,
    /// The capability's own documentation.
    pub capability_docs: String,
    params: Vec<Param>,
    /// JSON Schema of the arguments object.
    pub input_schema: Value,
    /// JSON Schema of the result, when the call returns a value.
    pub output_schema: Option<Value>,
}

#[derive(Clone, Debug)]
struct Param {
    /// snake_case, as in the arguments object.
    key: String,
    bytes: bool,
    optional: bool,
}

impl Method {
    /// `capability.method`.
    pub fn id(&self) -> String {
        format!("{}.{}", self.capability, self.name)
    }

    /// The method's name in Rust and on the wire: `transcribe_file`.
    pub fn rust_name(&self) -> String {
        snake(&self.name)
    }

    pub fn read_only(&self) -> bool {
        READ_ONLY.contains(&self.name.as_str()) || READ_ONLY_METHODS.contains(&self.id().as_str())
    }

    pub fn destructive(&self) -> bool {
        DESTRUCTIVE.contains(&self.name.as_str())
            || DESTRUCTIVE_METHODS.contains(&self.id().as_str())
    }

    /// Turns the arguments an agent sent (an object keyed by parameter
    /// name, or already an array) into the positional array the node's
    /// JSON dispatch takes. Strings given for `list<u8>` become their
    /// UTF-8 bytes. A missing optional argument is `null`.
    pub fn positional(&self, args: &Value) -> Result<Vec<Value>, String> {
        let values: Vec<Value> = match args {
            Value::Null => vec![Value::Null; self.params.len()],
            Value::Array(items) => items.clone(),
            Value::Object(map) => {
                if let Some(extra) = map
                    .keys()
                    .find(|k| !self.params.iter().any(|p| &p.key == *k))
                {
                    return Err(format!("{} takes no argument named {extra}", self.id()));
                }
                self.params
                    .iter()
                    .map(|p| map.get(&p.key).cloned().unwrap_or(Value::Null))
                    .collect()
            }
            _ => return Err("arguments must be an object or an array".into()),
        };
        if values.len() != self.params.len() {
            return Err(format!(
                "{} takes {} arguments, got {}",
                self.id(),
                self.params.len(),
                values.len()
            ));
        }
        self.params
            .iter()
            .zip(values)
            .map(|(p, v)| match v {
                Value::Null if !p.optional => {
                    Err(format!("{} needs the argument {}", self.id(), p.key))
                }
                Value::String(s) if p.bytes => Ok(json!(s.into_bytes())),
                v => Ok(v),
            })
            .collect()
    }

    /// What `search_capabilities` answers for this method.
    pub fn describe(&self) -> Value {
        let mut out = json!({
            "capability": self.capability,
            "method": self.name,
            "description": self.docs,
            "input_schema": self.input_schema,
            "read_only": self.read_only(),
            "destructive": self.destructive(),
        });
        if let Some(schema) = &self.output_schema {
            out["output_schema"] = schema.clone();
        }
        out
    }
}

/// Every method in the standard library.
#[derive(Clone, Debug)]
pub struct Catalog {
    methods: Vec<Method>,
}

impl Catalog {
    /// Parses the embedded standard library.
    pub fn stdlib() -> Result<Catalog, String> {
        Catalog::parse(STDLIB_WIT)
    }

    /// Parses one WIT package from `files`.
    pub fn parse(files: &[(&str, &str)]) -> Result<Catalog, String> {
        let mut map = SourceMap::new();
        for (name, text) in files {
            map.push_str(name, *text);
        }
        let group = map.parse().map_err(|(_, e)| e.to_string())?;
        let mut resolve = Resolve::new();
        resolve.push_group(group).map_err(|e| e.to_string())?;

        let mut methods = Vec::new();
        for (_, iface) in resolve.interfaces.iter() {
            let Some(capability) = &iface.name else {
                continue;
            };
            if capability == "types" {
                continue;
            }
            let capability_docs = iface.docs.contents.clone().unwrap_or_default();
            for (name, func) in &iface.functions {
                let mut properties = Map::new();
                let mut required = Vec::new();
                let mut params = Vec::new();
                for p in &func.params {
                    let key = snake(&p.name);
                    let schema = schema(&resolve, &p.ty, true);
                    let optional = is_option(&resolve, &p.ty);
                    if !optional {
                        required.push(json!(key));
                    }
                    params.push(Param {
                        key: key.clone(),
                        bytes: is_bytes(&resolve, &p.ty),
                        optional,
                    });
                    properties.insert(key, schema);
                }
                let input_schema = json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                });
                let output_schema = func
                    .result
                    .as_ref()
                    .and_then(|ty| ok_type(&resolve, ty))
                    .map(|ok| schema(&resolve, &ok, false));
                methods.push(Method {
                    capability: capability.clone(),
                    name: name.clone(),
                    docs: func.docs.contents.clone().unwrap_or_default(),
                    capability_docs: capability_docs.clone(),
                    params,
                    input_schema,
                    output_schema,
                });
            }
        }
        methods.sort_by_key(|m| m.id());
        Ok(Catalog { methods })
    }

    pub fn methods(&self) -> &[Method] {
        &self.methods
    }

    /// The method `capability.method`, named as in the WIT
    /// (`transcribe-file`) or as in Rust (`transcribe_file`).
    pub fn get(&self, capability: &str, method: &str) -> Option<&Method> {
        let method = method.replace('_', "-");
        self.methods
            .iter()
            .find(|m| m.capability == capability && m.name == method)
    }

    /// The methods named by `(capability, rust method name)` pairs, as a
    /// node reports what it can run. Pairs the WIT does not define are
    /// left out.
    pub fn select<'a>(&'a self, pairs: &[(String, String)]) -> Vec<&'a Method> {
        let mut out: Vec<&Method> = pairs
            .iter()
            .filter_map(|(cap, method)| self.get(cap, method))
            .collect();
        out.sort_by_key(|m| m.id());
        out.dedup_by_key(|m| m.id());
        out
    }
}

/// Whether `m` matches every word of `query` in its capability, name or
/// documentation, case-insensitively. An empty query matches.
pub fn matches(m: &Method, query: &str) -> bool {
    let hay = format!(
        "{} {} {} {}",
        m.capability, m.name, m.docs, m.capability_docs
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

fn snake(name: &str) -> String {
    name.replace('-', "_")
}

fn def(resolve: &Resolve, id: TypeId) -> &TypeDefKind {
    &resolve.types[id].kind
}

/// Follows `type x = y` aliases.
fn unalias(resolve: &Resolve, ty: &Type) -> Type {
    let mut ty = *ty;
    while let Type::Id(id) = ty {
        match def(resolve, id) {
            TypeDefKind::Type(inner) => ty = *inner,
            _ => break,
        }
    }
    ty
}

fn is_option(resolve: &Resolve, ty: &Type) -> bool {
    matches!(unalias(resolve, ty), Type::Id(id) if matches!(def(resolve, id), TypeDefKind::Option(_)))
}

fn is_bytes(resolve: &Resolve, ty: &Type) -> bool {
    matches!(unalias(resolve, ty), Type::Id(id)
        if matches!(def(resolve, id), TypeDefKind::List(inner) if unalias(resolve, inner) == Type::U8))
}

/// The `ok` side of a `result<T, call-error>`, or the type itself.
fn ok_type(resolve: &Resolve, ty: &Type) -> Option<Type> {
    match unalias(resolve, ty) {
        Type::Id(id) => match def(resolve, id) {
            TypeDefKind::Result(r) => r.ok,
            _ => Some(*ty),
        },
        other => Some(other),
    }
}

fn int(min: i128, max: i128) -> Value {
    json!({ "type": "integer", "minimum": min, "maximum": max })
}

/// The JSON Schema of `ty` as serde encodes it. `input` lets `list<u8>`
/// take a string too.
fn schema(resolve: &Resolve, ty: &Type, input: bool) -> Value {
    match unalias(resolve, ty) {
        Type::Bool => json!({ "type": "boolean" }),
        Type::U8 => int(0, u8::MAX.into()),
        Type::U16 => int(0, u16::MAX.into()),
        Type::U32 => int(0, u32::MAX.into()),
        Type::U64 => int(0, u64::MAX.into()),
        Type::S8 => int(i8::MIN.into(), i8::MAX.into()),
        Type::S16 => int(i16::MIN.into(), i16::MAX.into()),
        Type::S32 => int(i32::MIN.into(), i32::MAX.into()),
        Type::S64 => int(i64::MIN.into(), i64::MAX.into()),
        Type::F32 | Type::F64 => json!({ "type": "number" }),
        Type::Char => json!({ "type": "string", "minLength": 1, "maxLength": 1 }),
        Type::String => json!({ "type": "string" }),
        Type::ErrorContext => json!({}),
        Type::Id(id) => {
            let docs = resolve.types[id].docs.contents.clone();
            let mut out = match def(resolve, id) {
                TypeDefKind::Record(r) => {
                    let mut properties = Map::new();
                    let mut required = Vec::new();
                    for f in &r.fields {
                        let key = snake(&f.name);
                        let mut s = schema(resolve, &f.ty, input);
                        if let (Some(d), Some(obj)) = (&f.docs.contents, s.as_object_mut()) {
                            obj.insert("description".into(), json!(d));
                        }
                        if !is_option(resolve, &f.ty) {
                            required.push(json!(key));
                        }
                        properties.insert(key, s);
                    }
                    json!({ "type": "object", "properties": properties, "required": required })
                }
                TypeDefKind::Option(inner) => {
                    json!({ "anyOf": [schema(resolve, inner, input), { "type": "null" }] })
                }
                TypeDefKind::List(inner) if unalias(resolve, inner) == Type::U8 => {
                    let bytes = json!({ "type": "array", "items": int(0, 255) });
                    if input {
                        json!({
                            "anyOf": [{ "type": "string", "description": "UTF-8 text" }, bytes]
                        })
                    } else {
                        bytes
                    }
                }
                TypeDefKind::List(inner) => {
                    json!({ "type": "array", "items": schema(resolve, inner, input) })
                }
                TypeDefKind::FixedLengthList(inner, n) => json!({
                    "type": "array",
                    "items": schema(resolve, inner, input),
                    "minItems": n,
                    "maxItems": n,
                }),
                TypeDefKind::Tuple(t) => json!({
                    "type": "array",
                    "prefixItems": t.types.iter().map(|t| schema(resolve, t, input)).collect::<Vec<_>>(),
                    "minItems": t.types.len(),
                    "maxItems": t.types.len(),
                }),
                TypeDefKind::Enum(e) => json!({
                    "type": "string",
                    "enum": e.cases.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                }),
                TypeDefKind::Flags(f) => json!({
                    "type": "array",
                    "items": { "enum": f.flags.iter().map(|c| c.name.clone()).collect::<Vec<_>>() },
                }),
                TypeDefKind::Result(r) => match r.ok {
                    Some(ok) => schema(resolve, &ok, input),
                    None => json!({ "type": "null" }),
                },
                // Variants, resources, maps and async types: no schema yet.
                _ => json!({}),
            };
            if let (Some(d), Some(obj)) = (docs, out.as_object_mut())
                && !obj.contains_key("description")
            {
                obj.insert("description".into(), json!(d));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog::stdlib().unwrap()
    }

    #[test]
    fn embedded_wit_matches_the_wit_folder() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".wit"))
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = STDLIB_WIT.iter().map(|(n, _)| n.to_string()).collect();
        embedded.sort();
        assert_eq!(embedded, on_disk);
        for (name, text) in STDLIB_WIT {
            let disk = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
            assert_eq!(&disk, text, "{name} is stale");
        }
    }

    #[test]
    fn parses_every_interface_with_docs_and_schemas() {
        let c = catalog();
        assert!(c.methods().len() > 40, "{}", c.methods().len());
        let read = c.get("disk", "read").unwrap();
        assert!(read.read_only() && !read.destructive());
        assert_eq!(read.input_schema["required"], json!(["path"]));
        assert_eq!(read.input_schema["properties"]["path"]["type"], "string");
        assert_eq!(read.output_schema.as_ref().unwrap()["type"], "array");

        let write = c.get("disk", "write").unwrap();
        assert!(write.destructive());
        assert!(write.docs.contains("read-write grant"));

        let speak = c.get("tts", "speak").unwrap();
        assert_eq!(speak.input_schema["required"], json!(["text"]));
        let status = c.get("tts", "status").unwrap();
        let out = status.output_schema.as_ref().unwrap();
        assert_eq!(out["properties"]["queued"]["type"], "integer");

        assert!(c.get("claude-code", "start").unwrap().destructive());
        assert!(c.get("claude-code", "status").unwrap().read_only());

        // The computer-use capabilities: killing, sleeping, locking, Wi-Fi
        // off and scripts ask first; reading the tree and the device does not.
        for (cap, m) in [
            ("process", "kill"),
            ("process", "stop"),
            ("device", "sleep"),
            ("device", "lock"),
            ("device", "set-wifi"),
            ("scripting", "run-applescript"),
            ("scripting", "run-powershell"),
            ("virtual-screen", "destroy"),
            ("clipboard", "set-image"),
        ] {
            assert!(c.get(cap, m).unwrap().destructive(), "{cap}.{m}");
        }
        for (cap, m) in [
            ("accessibility", "snapshot"),
            ("accessibility", "list-apps"),
            ("accessibility", "find"),
            ("accessibility", "wait"),
            ("process", "list"),
            ("device", "volume"),
            ("clipboard", "get-image"),
        ] {
            let method = c.get(cap, m).unwrap();
            assert!(method.read_only() && !method.destructive(), "{cap}.{m}");
        }
        let click = c.get("accessibility", "click").unwrap();
        assert!(!click.read_only() && !click.destructive());
        assert!(!c.get("window", "focus").unwrap().read_only());

        let file = c.get("speech", "transcribe_file").unwrap();
        assert_eq!(file.name, "transcribe-file");
        assert_eq!(file.rust_name(), "transcribe_file");
    }

    #[test]
    fn select_keeps_known_pairs_once() {
        let c = catalog();
        let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        let got: Vec<String> = c
            .select(&pairs(&[
                ("tts", "speak"),
                ("speech", "transcribe"),
                ("tts", "speak"),
                ("nope", "x"),
            ]))
            .iter()
            .map(|m| m.id())
            .collect();
        assert_eq!(got, ["speech.transcribe", "tts.speak"]);
    }

    #[test]
    fn positional_orders_named_args_and_encodes_bytes() {
        let c = catalog();
        let write = c.get("disk", "write").unwrap();
        let args = write
            .positional(&json!({ "data": "hi", "path": "~/a.txt" }))
            .unwrap();
        assert_eq!(args, vec![json!("~/a.txt"), json!([104, 105])]);

        let speak = c.get("tts", "speak").unwrap();
        assert_eq!(
            speak.positional(&json!({ "text": "hello" })).unwrap(),
            vec![json!("hello"), Value::Null]
        );
        assert!(speak.positional(&json!({})).is_err());
        assert!(speak.positional(&json!({ "txt": "x" })).is_err());
        assert_eq!(
            c.get("clipboard", "get")
                .unwrap()
                .positional(&Value::Null)
                .unwrap(),
            Vec::<Value>::new()
        );
        assert!(write.positional(&json!(["only one"])).is_err());
    }

    #[test]
    fn matches_words_in_names_and_docs() {
        let c = catalog();
        assert!(matches(c.get("tts", "speak").unwrap(), "speech text"));
        assert!(matches(c.get("clipboard", "get").unwrap(), ""));
        assert!(!matches(c.get("clipboard", "get").unwrap(), "window"));
    }
}
