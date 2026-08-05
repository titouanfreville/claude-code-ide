//! gRPC client core behind the gRPC request builder (center tab) — the sibling of
//! [`http`](crate::http), and **GPUI-free** for the same reason: it unit-tests without a
//! UI.
//!
//! The shape that makes this different from HTTP: protobuf messages are not
//! self-describing, so before a call can be made at all the tool must *learn the schema*.
//! Two sources, in preference order:
//!
//! 1. **Server reflection** ([`schema_from_reflection`]) — ask the server itself over
//!    `grpc.reflection.v1.ServerReflection`, falling back to the older `v1alpha` service
//!    that many deployments still expose. Zero configuration: point at a host and browse.
//! 2. **Local `.proto` files** ([`schema_from_protos`]) — compiled in-process by
//!    `protox`, a pure-Rust `protoc`. Covers servers with reflection disabled (common in
//!    prod) and services that aren't running yet.
//!
//! Either way the result is a [`DescriptorPool`], and every message is then built and
//! read as a [`DynamicMessage`] — protobuf assembled at runtime from a descriptor rather
//! than from build-time-generated structs. That is why this module carries its own
//! [`DynamicCodec`] instead of using tonic's generated-stub codec: the decoder has to
//! *hold* the response descriptor, because a `DynamicMessage` cannot be default-built.
//!
//! Security reuses the HTTP tool's gate verbatim — [`http::host_allowed`] already parses
//! a bare `host:port` authority correctly, so a gRPC target is scoped by exactly the same
//! `allowed_hosts` rule, from exactly the same environment manifest.
//!
//! Scope: **unary calls only**. Streaming methods are surfaced by [`Schema::methods`] but
//! flagged via [`MethodInfo::streaming`] so the UI can show and disable them. The one
//! streaming code path here is reflection itself, which is bidirectional by spec.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use prost::Message as _;
// `prost_types` comes via prost-reflect's re-export, so the descriptor types here are
// guaranteed to be the same ones the pool accepts (no second prost-types version).
use prost_reflect::prost_types;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, SerializeOptions};
use serde::{Deserialize, Serialize};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::{Request, Status};

use crate::http;

/// Cap on retained history records — oldest drop off the front (mirrors `HttpHistory`).
const HISTORY_CAP: usize = 50;
/// Response JSON kept for the panel's response pane.
const PANEL_BODY: usize = 200_000;
/// Per-call timeout (connect + request).
const TIMEOUT: Duration = Duration::from_secs(15);
/// Connect timeout, kept short so a wrong port fails fast instead of hanging the panel.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The reflection service paths, tried in order. `v1` is the standardised service;
/// `v1alpha` is the long-lived original that a large share of deployed servers (and
/// every older grpc-go/grpc-java) still expose under its own name. The message wire
/// format is identical between them, so the same types decode both.
const REFLECTION_V1: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
const REFLECTION_V1ALPHA: &str = "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

/// Where to send the call: an authority (`host:port`) plus whether to use TLS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrpcTarget {
    pub authority: String,
    pub tls: bool,
}

impl GrpcTarget {
    /// The URL tonic's `Endpoint` wants. gRPC schemes map onto HTTP ones: the transport
    /// underneath is plain HTTP/2 either way.
    pub fn endpoint_url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.authority)
    }
}

/// Parse an operator-typed target. Accepts a bare `host:port` (plaintext — the common
/// dev case), or an explicit `grpc://`/`http://` (plaintext) or `grpcs://`/`https://`
/// (TLS) URL.
///
/// A bare host with **no port** is rejected rather than guessed at: gRPC has no
/// well-known default port, so silently picking one would produce a confusing connection
/// error instead of a clear message.
pub fn parse_target(raw: &str) -> Result<GrpcTarget, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("enter a target, e.g. localhost:50051".into());
    }
    let (tls, rest) = match raw.split_once("://") {
        Some(("grpc" | "http", rest)) => (false, rest),
        Some(("grpcs" | "https", rest)) => (true, rest),
        Some((scheme, _)) => return Err(format!("unsupported scheme `{scheme}://`")),
        None => (false, raw),
    };
    // Drop any path/query the operator pasted — a gRPC target is an authority only.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .trim()
        .to_string();
    if authority.is_empty() {
        return Err("target has no host".into());
    }
    // An explicit scheme implies the standard port; a bare target must say which port.
    let has_port = authority
        .rsplit(':')
        .next()
        .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    let authority = if has_port {
        authority
    } else if raw.contains("://") {
        format!("{authority}:{}", if tls { 443 } else { 80 })
    } else {
        return Err(format!(
            "`{authority}` needs a port — gRPC has no default (e.g. {authority}:50051)"
        ));
    };
    Ok(GrpcTarget { authority, tls })
}

// ---------------------------------------------------------------------------
// Reflection wire types
// ---------------------------------------------------------------------------

/// The subset of `grpc/reflection/v1/reflection.proto` this tool actually uses.
///
/// Hand-declared rather than generated: `tonic-reflection` ships a *server* only (it has
/// no `client` feature), so the alternative would be vendoring the `.proto` plus a
/// `build.rs` and a codegen build-dependency — a lot of machinery for six small messages.
/// Fields we never read (e.g. `original_request`) are simply omitted; protobuf skips
/// unknown fields on decode, so this stays wire-compatible with the full definition.
pub mod reflection {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ServerReflectionRequest {
        #[prost(string, tag = "1")]
        pub host: String,
        #[prost(oneof = "MessageRequest", tags = "3, 4, 7")]
        pub message_request: Option<MessageRequest>,
    }

    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageRequest {
        #[prost(string, tag = "3")]
        FileByFilename(String),
        #[prost(string, tag = "4")]
        FileContainingSymbol(String),
        /// The value is unused by the protocol — servers ignore it.
        #[prost(string, tag = "7")]
        ListServices(String),
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ServerReflectionResponse {
        #[prost(string, tag = "1")]
        pub valid_host: String,
        #[prost(oneof = "MessageResponse", tags = "4, 6, 7")]
        pub message_response: Option<MessageResponse>,
    }

    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageResponse {
        #[prost(message, tag = "4")]
        FileDescriptor(FileDescriptorResponse),
        #[prost(message, tag = "6")]
        ListServices(ListServiceResponse),
        #[prost(message, tag = "7")]
        Error(ErrorResponse),
    }

    /// Serialised `FileDescriptorProto`s — the file that contains the requested symbol,
    /// plus (per spec) its transitive dependencies.
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct FileDescriptorResponse {
        #[prost(bytes = "vec", repeated, tag = "1")]
        pub file_descriptor_proto: Vec<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ListServiceResponse {
        #[prost(message, repeated, tag = "1")]
        pub service: Vec<ServiceResponse>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ServiceResponse {
        #[prost(string, tag = "1")]
        pub name: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ErrorResponse {
        #[prost(int32, tag = "1")]
        pub error_code: i32,
        #[prost(string, tag = "2")]
        pub error_message: String,
    }
}

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// Codec over ordinary generated prost types — used for the reflection call.
///
/// tonic's own prost codec lives in the separate `tonic-prost` crate, which we don't
/// depend on (we need the dynamic codec below regardless), so this supplies the same
/// few lines for the one statically-typed call this module makes.
pub struct PbCodec<E, D>(PhantomData<(E, D)>);

impl<E, D> Default for PbCodec<E, D> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<E, D> Codec for PbCodec<E, D>
where
    E: prost::Message + Send + 'static,
    D: prost::Message + Default + Send + 'static,
{
    type Encode = E;
    type Decode = D;
    type Encoder = PbEncoder<E>;
    type Decoder = PbDecoder<D>;

    fn encoder(&mut self) -> Self::Encoder {
        PbEncoder(PhantomData)
    }
    fn decoder(&mut self) -> Self::Decoder {
        PbDecoder(PhantomData)
    }
}

pub struct PbEncoder<E>(PhantomData<E>);

impl<E: prost::Message> Encoder for PbEncoder<E> {
    type Item = E;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("encode: {e}")))
    }
}

pub struct PbDecoder<D>(PhantomData<D>);

impl<D: prost::Message + Default> Decoder for PbDecoder<D> {
    type Item = D;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        D::decode(src)
            .map(Some)
            .map_err(|e| Status::internal(format!("decode: {e}")))
    }
}

/// Codec over [`DynamicMessage`] — the one that makes runtime-discovered calls possible.
///
/// The decoder must **own the response [`MessageDescriptor`]**: unlike a generated struct
/// a `DynamicMessage` has no `Default`, so there is nothing to decode *into* without the
/// descriptor in hand. That single constraint is the reason a stock codec cannot be used
/// for a method whose type is only known at runtime.
pub struct DynamicCodec {
    response: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(response: MessageDescriptor) -> Self {
        Self { response }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynEncoder;
    type Decoder = DynDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynEncoder
    }
    fn decoder(&mut self) -> Self::Decoder {
        DynDecoder(self.response.clone())
    }
}

pub struct DynEncoder;

impl Encoder for DynEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("encode request: {e}")))
    }
}

pub struct DynDecoder(MessageDescriptor);

impl Decoder for DynDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let mut msg = DynamicMessage::new(self.0.clone());
        msg.merge(src)
            .map_err(|e| Status::internal(format!("decode response: {e}")))?;
        Ok(Some(msg))
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A method the panel can list: its fully-qualified path plus enough shape for the UI to
/// decide whether it is callable in this increment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodInfo {
    /// Bare method name, e.g. `GetUser`.
    pub name: String,
    /// The gRPC path, e.g. `/pkg.UserService/GetUser`.
    pub path: String,
    /// `Some(kind)` for a streaming method — listed but not callable yet.
    pub streaming: Option<&'static str>,
}

impl MethodInfo {
    pub fn is_unary(&self) -> bool {
        self.streaming.is_none()
    }
}

/// A discovered protobuf schema — services, methods, and the message types behind them.
#[derive(Clone, Debug)]
pub struct Schema {
    pool: DescriptorPool,
}

impl Schema {
    pub fn from_pool(pool: DescriptorPool) -> Self {
        Self { pool }
    }

    /// Fully-qualified service names, sorted. The server's own reflection and health
    /// services are kept: calling `grpc.health.v1.Health/Check` from the panel is a
    /// legitimate thing to want.
    pub fn services(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .pool
            .services()
            .map(|s| s.full_name().to_string())
            .collect();
        names.sort();
        names
    }

    /// The methods of `service`, in declaration order.
    pub fn methods(&self, service: &str) -> Vec<MethodInfo> {
        let Some(svc) = self.pool.get_service_by_name(service) else {
            return Vec::new();
        };
        svc.methods()
            .map(|m| MethodInfo {
                name: m.name().to_string(),
                path: format!("/{}/{}", svc.full_name(), m.name()),
                streaming: match (m.is_client_streaming(), m.is_server_streaming()) {
                    (false, false) => None,
                    (true, false) => Some("client streaming"),
                    (false, true) => Some("server streaming"),
                    (true, true) => Some("bidirectional streaming"),
                },
            })
            .collect()
    }

    /// The request/response message descriptors for `service`/`method`.
    pub fn method_types(
        &self,
        service: &str,
        method: &str,
    ) -> Option<(MessageDescriptor, MessageDescriptor)> {
        let svc = self.pool.get_service_by_name(service)?;
        let m = svc.methods().find(|m| m.name() == method)?;
        Some((m.input(), m.output()))
    }

    /// A skeleton JSON object for a request message — the affordance that replaces
    /// "knowing the proto". Every field is present with a zero value of the right shape,
    /// so the operator edits rather than authors.
    pub fn example_json(&self, desc: &MessageDescriptor) -> String {
        let value = example_value(desc, 0);
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
    }
}

/// Build a placeholder JSON value for every field of `desc`. `depth` guards against
/// self-referential message types, which are legal protobuf and would otherwise recurse
/// forever.
fn example_value(desc: &MessageDescriptor, depth: usize) -> serde_json::Value {
    use prost_reflect::Kind;

    let mut map = serde_json::Map::new();
    if depth > 4 {
        return serde_json::Value::Object(map);
    }
    for field in desc.fields() {
        // Protobuf JSON uses lowerCamelCase names by default.
        let name = field.json_name().to_string();
        let scalar = match field.kind() {
            Kind::Double | Kind::Float => serde_json::json!(0.0),
            Kind::Int32
            | Kind::Sint32
            | Kind::Sfixed32
            | Kind::Int64
            | Kind::Sint64
            | Kind::Sfixed64
            | Kind::Uint32
            | Kind::Fixed32
            | Kind::Uint64
            | Kind::Fixed64 => serde_json::json!(0),
            Kind::Bool => serde_json::json!(false),
            Kind::String => serde_json::json!(""),
            Kind::Bytes => serde_json::json!(""),
            // Enums serialise as their variant name in protobuf JSON.
            Kind::Enum(e) => e
                .values()
                .next()
                .map(|v| serde_json::json!(v.name()))
                .unwrap_or(serde_json::Value::Null),
            Kind::Message(m) => example_value(&m, depth + 1),
        };
        let value = if field.is_map() {
            serde_json::Value::Object(serde_json::Map::new())
        } else if field.is_list() {
            serde_json::Value::Array(vec![scalar])
        } else {
            scalar
        };
        map.insert(name, value);
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// Request form
// ---------------------------------------------------------------------------

/// How deep the form flattens nested messages before falling back to a raw-JSON cell.
/// Past this, a table stops being clearer than the JSON it replaced — and protobuf allows
/// self-referential types, which would otherwise flatten forever.
const MAX_FORM_DEPTH: usize = 3;

/// How one field is edited in the request form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormInput {
    /// A free-text cell. `numeric` decides whether typed text is emitted as a JSON number
    /// or a JSON string.
    Text { numeric: bool },
    /// Unset / true / false.
    Bool,
    /// Unset, or one of the enum's variant names.
    Enum(Vec<String>),
    /// Repeated fields, maps, and messages too deep to flatten: one cell of raw JSON.
    /// `hint` is a shape example shown as the placeholder.
    Json { hint: String },
    /// A nested message — a header with the message's own fields below it, never a value
    /// of its own.
    Group,
}

/// One row of the request form: a field of the request message, flattened with its path.
///
/// This is the payoff of reflection. The server already told us every field, its type,
/// and whether it repeats — so the operator picks values instead of retyping that shape
/// as JSON from memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormField {
    /// Dotted path from the root message in protobuf-JSON names, e.g. `profile.bio`.
    /// Unique per row, and the key the panel stores the cell's text under.
    pub path: String,
    /// The same path in the `.proto`'s own field names (`profile.display_name`).
    /// Protobuf JSON accepts either spelling, so reading a message back into the form
    /// has to try both or silently drop hand-written snake_case fields.
    pub alt_path: String,
    /// The leaf name, as protobuf JSON spells it.
    pub name: String,
    /// Nesting level; 0 for a field of the request message itself.
    pub depth: usize,
    /// The declared type, spelled as the Schema tab spells it.
    pub type_label: String,
    pub input: FormInput,
}

impl FormField {
    /// Whether this row holds a value (a group is only a header).
    pub fn is_editable(&self) -> bool {
        !matches!(self.input, FormInput::Group)
    }
}

impl Schema {
    /// Flatten a message into editable rows, depth-first, in field-declaration order.
    pub fn form_fields(&self, desc: &MessageDescriptor) -> Vec<FormField> {
        let mut out = Vec::new();
        collect_form_fields(desc, "", "", 0, &mut out);
        out
    }
}

fn collect_form_fields(
    desc: &MessageDescriptor,
    prefix: &str,
    alt_prefix: &str,
    depth: usize,
    out: &mut Vec<FormField>,
) {
    use prost_reflect::Kind;

    let join = |prefix: &str, name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        }
    };

    for field in desc.fields() {
        let name = field.json_name().to_string();
        let path = join(prefix, &name);
        let alt_path = join(alt_prefix, field.name());
        let type_label = field_kind_label(&field);

        // Maps first: a map field is also repeated, so the order of these two tests is
        // what keeps `map<string,string>` from being read as a list.
        let input = if field.is_map() {
            FormInput::Json {
                hint: r#"{"key": "value"}"#.to_string(),
            }
        } else if field.is_list() {
            FormInput::Json {
                hint: list_hint(&field.kind()),
            }
        } else {
            match field.kind() {
                Kind::Bool => FormInput::Bool,
                Kind::Enum(e) => {
                    FormInput::Enum(e.values().map(|v| v.name().to_string()).collect())
                }
                // A nested message becomes a group with its own rows — unless it would
                // nest too deep, or has no fields of its own to show.
                Kind::Message(m) if depth < MAX_FORM_DEPTH && m.fields().next().is_some() => {
                    out.push(FormField {
                        path: path.clone(),
                        alt_path: alt_path.clone(),
                        name,
                        depth,
                        type_label,
                        input: FormInput::Group,
                    });
                    collect_form_fields(&m, &path, &alt_path, depth + 1, out);
                    continue;
                }
                Kind::Message(_) => FormInput::Json {
                    hint: "{ … }".to_string(),
                },
                Kind::Double
                | Kind::Float
                | Kind::Int32
                | Kind::Sint32
                | Kind::Sfixed32
                | Kind::Int64
                | Kind::Sint64
                | Kind::Sfixed64
                | Kind::Uint32
                | Kind::Fixed32
                | Kind::Uint64
                | Kind::Fixed64 => FormInput::Text { numeric: true },
                Kind::String | Kind::Bytes => FormInput::Text { numeric: false },
            }
        };

        out.push(FormField {
            path,
            alt_path,
            name,
            depth,
            type_label,
            input,
        });
    }
}

/// A one-element sample of a repeated field's element type, used as the cell placeholder.
fn list_hint(kind: &prost_reflect::Kind) -> String {
    use prost_reflect::Kind;
    match kind {
        Kind::Bool => "[true]".into(),
        Kind::String | Kind::Bytes => r#"["…"]"#.into(),
        Kind::Message(_) => "[{ … }]".into(),
        Kind::Enum(e) => e
            .values()
            .next()
            .map(|v| format!("[\"{}\"]", v.name()))
            .unwrap_or_else(|| r#"["…"]"#.into()),
        _ => "[0]".into(),
    }
}

/// A human-readable type for a field, as the Schema tab and the form's type column show
/// it.
pub fn field_kind_label(field: &prost_reflect::FieldDescriptor) -> String {
    use prost_reflect::Kind;
    let base = match field.kind() {
        Kind::Double => "double".to_string(),
        Kind::Float => "float".to_string(),
        Kind::Int32 => "int32".to_string(),
        Kind::Int64 => "int64".to_string(),
        Kind::Uint32 => "uint32".to_string(),
        Kind::Uint64 => "uint64".to_string(),
        Kind::Sint32 => "sint32".to_string(),
        Kind::Sint64 => "sint64".to_string(),
        Kind::Fixed32 => "fixed32".to_string(),
        Kind::Fixed64 => "fixed64".to_string(),
        Kind::Sfixed32 => "sfixed32".to_string(),
        Kind::Sfixed64 => "sfixed64".to_string(),
        Kind::Bool => "bool".to_string(),
        Kind::String => "string".to_string(),
        Kind::Bytes => "bytes".to_string(),
        Kind::Message(m) => m.full_name().to_string(),
        Kind::Enum(e) => e.full_name().to_string(),
    };
    if field.is_map() {
        // The entry message's own two fields are the map's key and value types.
        let pair = match field.kind() {
            Kind::Message(entry) => {
                let mut fields = entry.fields();
                match (fields.next(), fields.next()) {
                    (Some(k), Some(v)) => Some(format!(
                        "{}, {}",
                        field_kind_label(&k),
                        field_kind_label(&v)
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        format!("map<{}>", pair.unwrap_or(base))
    } else if field.is_list() {
        format!("repeated {base}")
    } else {
        base
    }
}

/// Assemble the form's cells into a protobuf-JSON message.
///
/// An empty cell is **omitted entirely** rather than sent as a zero. In proto3 an unset
/// field and a zero-valued one are indistinguishable on the wire, but they are not
/// indistinguishable to a server that treats `""` as "clear this" — so the form's default
/// of `null` has to mean "I did not touch this", and only omission says that.
pub fn build_message_json(fields: &[FormField], values: &BTreeMap<String, String>) -> String {
    let mut root = serde_json::Map::new();
    for field in fields.iter().filter(|f| f.is_editable()) {
        let Some(raw) = values.get(&field.path) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let value = match &field.input {
            FormInput::Bool => serde_json::Value::Bool(raw == "true"),
            FormInput::Enum(_) => serde_json::Value::String(raw.to_string()),
            // A cell that doesn't parse is passed through as a string rather than
            // dropped: the request then fails naming *this* field, which is a far better
            // error than a silently missing one.
            FormInput::Json { .. } => serde_json::from_str(raw)
                .unwrap_or_else(|_| serde_json::Value::String(raw.to_string())),
            // Text that parses as a number is emitted as one; anything else stays a
            // string, which also lets `{{vars}}` survive to interpolation (and is the
            // spec's own form for 64-bit integers).
            FormInput::Text { numeric: true } => match serde_json::from_str(raw) {
                Ok(serde_json::Value::Number(n)) => serde_json::Value::Number(n),
                _ => serde_json::Value::String(raw.to_string()),
            },
            FormInput::Text { numeric: false } => serde_json::Value::String(raw.to_string()),
            FormInput::Group => continue,
        };
        insert_at_path(&mut root, &field.path, value);
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(root)).unwrap_or_else(|_| "{}".into())
}

/// Read a JSON message back into form cells, so switching JSON → Form, loading a saved
/// call, or filling the example all land in the table rather than blanking it.
pub fn seed_form_values(fields: &[FormField], json: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    for field in fields.iter().filter(|f| f.is_editable()) {
        let found =
            value_at_path(&root, &field.path).or_else(|| value_at_path(&root, &field.alt_path));
        if let Some(value) = found {
            let text = cell_text(value);
            if !text.is_empty() {
                out.insert(field.path.clone(), text);
            }
        }
    }
    out
}

/// Write `value` at a dotted path, creating the intermediate objects a nested field needs.
fn insert_at_path(
    root: &mut serde_json::Map<String, serde_json::Value>,
    path: &str,
    value: serde_json::Value,
) {
    let parts: Vec<&str> = path.split('.').collect();
    let Some((leaf, parents)) = parts.split_last() else {
        return;
    };
    let mut cursor = root;
    for part in parents {
        let slot = cursor
            .entry((*part).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !slot.is_object() {
            *slot = serde_json::Value::Object(serde_json::Map::new());
        }
        cursor = slot.as_object_mut().expect("set to an object just above");
    }
    cursor.insert((*leaf).to_string(), value);
}

fn value_at_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cursor = root;
    for part in path.split('.') {
        cursor = cursor.as_object()?.get(part)?;
    }
    Some(cursor)
}

/// How a JSON value reads as cell text. Strings lose their quotes (the cell is already a
/// text field); everything structural keeps its JSON form, because that is what the cell
/// accepts back.
fn cell_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// A failure in three parts: what failed, what the system said, and what to do about it.
///
/// A bare error string makes the operator fluent in gRPC's vocabulary before they can act
/// — `transport error` and `status: Unimplemented` are both technically accurate and
/// practically useless. The hint is where this tool's knowledge of *its own*
/// configuration goes, since it is the only party that knows about `allowed_hosts`,
/// `proto_files`, and the Connect button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrpcError {
    /// What failed, in plain words — the line the operator reads first.
    pub title: String,
    /// What the underlying library actually said, verbatim.
    pub detail: String,
    /// The next action, when the tool can name one.
    pub hint: Option<String>,
}

impl GrpcError {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// One-line form, for the history list where there is no room for three parts.
    pub fn one_line(&self) -> String {
        if self.detail.is_empty() {
            self.title.clone()
        } else {
            format!("{}: {}", self.title, self.detail)
        }
    }

    /// The copyable form — what lands on the clipboard, and what belongs in a bug report.
    /// Keeps all three parts, since the hint is often the part worth quoting.
    pub fn as_block(&self) -> String {
        let mut out = self.title.clone();
        if !self.detail.is_empty() {
            out.push('\n');
            out.push_str(&self.detail);
        }
        if let Some(hint) = &self.hint {
            out.push_str("\n→ ");
            out.push_str(hint);
        }
        out
    }
}

/// The value on one line of a pretty-printed JSON response, ready to paste straight into
/// a request cell: unquoted, unescaped, and without the trailing comma.
///
/// Lines that open an object or array have no scalar value of their own, so they copy as
/// they read — a predictable rule beats a clever one that sometimes returns `{`.
pub fn json_line_value(line: &str) -> String {
    let trimmed = line.trim();
    let after_key = match trimmed.split_once("\": ") {
        Some((_, rest)) => rest,
        // Array elements have no key, so the whole line is the value.
        None => trimmed,
    };
    let value = after_key.trim_end_matches(',').trim();
    if value.is_empty() || value == "{" || value == "[" {
        return trimmed.to_string();
    }
    // Unescape, so `"a\nb"` reaches the clipboard as the two lines it stands for.
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_string());
    }
    value.to_string()
}

/// Flatten an error and everything that caused it into one line.
///
/// This is the single biggest legibility win available here: tonic's transport errors
/// render as the bare word `transport error`, and every fact worth having — connection
/// refused, DNS failure, certificate mismatch — lives in the `source()` chain underneath.
pub fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        // Skip repeats: wrapper types often restate their cause verbatim.
        if !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(" → ")
}

/// What a gRPC status code usually means *for the operator of this panel*.
///
/// Deliberately not the spec's definition, which the code name already conveys. These
/// name the most likely local cause, because a status code arriving in a request builder
/// is nearly always about how the request was built or where it was pointed.
pub fn status_hint(code: tonic::Code) -> Option<&'static str> {
    use tonic::Code;
    Some(match code {
        Code::Unimplemented => {
            "the server has no such method — check the service and method names, and that \
             this target is the server that implements them"
        }
        Code::Unauthenticated => {
            "the server wants credentials — add an `authorization` metadata row"
        }
        Code::PermissionDenied => "authenticated, but not allowed to call this method",
        Code::InvalidArgument => "the server rejected the request message — check the field values",
        Code::NotFound => "the call was well-formed; the server has no such record",
        Code::DeadlineExceeded => "the server did not answer within the 15s call timeout",
        Code::Unavailable => {
            "the server is reachable but not serving — it may still be starting, or the \
             port may belong to a different process"
        }
        Code::ResourceExhausted => "a server-side quota or rate limit rejected the call",
        Code::FailedPrecondition => "the server's state does not allow this call right now",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Schema sources
// ---------------------------------------------------------------------------

/// Build a channel to `target`. Kept separate so both reflection and the call itself
/// connect the same way.
async fn connect(target: &GrpcTarget) -> Result<tonic::transport::Channel, GrpcError> {
    let mut endpoint = tonic::transport::Endpoint::from_shared(target.endpoint_url())
        .map_err(|e| GrpcError::new("Invalid target", error_chain(&e)))?
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TIMEOUT);
    if target.tls {
        endpoint = endpoint
            .tls_config(tonic::transport::ClientTlsConfig::new().with_enabled_roots())
            .map_err(|e| GrpcError::new("TLS setup failed", error_chain(&e)))?;
    }
    endpoint.connect().await.map_err(|e| {
        let detail = error_chain(&e);
        let hint = if !target.tls && detail.contains("frame") {
            // A plaintext client against a TLS port reads the ServerHello as a corrupt
            // HTTP/2 frame — a confusing failure with a one-word fix.
            Some(format!(
                "if {} serves TLS, address it as grpcs://{}",
                target.authority, target.authority
            ))
        } else if detail.contains("certificate") || detail.contains("CaUsedAsEndEntity") {
            Some("the server's certificate was not trusted by the system roots".to_string())
        } else {
            Some(format!(
                "nothing is accepting connections on {} — check the port, and that the \
                 server is running",
                target.authority
            ))
        };
        GrpcError {
            title: format!("Could not connect to {}", target.authority),
            detail,
            hint,
        }
    })
}

/// Discover the server's schema over gRPC server reflection.
///
/// Tries the standardised `v1` service, then `v1alpha` — a server that implements only
/// the older one answers `Unimplemented` on `v1`, and a great many deployed servers are
/// still in that position. When neither is present the error names the fix, because a
/// missing reflection service is a *server configuration* fact the operator has to act on
/// (or work around with `.proto` files).
pub async fn schema_from_reflection(target: &GrpcTarget) -> Result<Schema, GrpcError> {
    let channel = connect(target).await?;
    let first = reflect_over(&channel, REFLECTION_V1).await;
    let err = match first {
        Ok(schema) => return Ok(schema),
        Err(e) => e,
    };
    match reflect_over(&channel, REFLECTION_V1ALPHA).await {
        Ok(schema) => Ok(schema),
        // Report the v1 failure: it is the service a modern server should expose, so its
        // error is the more actionable of the two. The connection itself worked, so this
        // is a server *configuration* fact, not a reachability one.
        Err(_) => Err(GrpcError::new(
            format!("{} does not expose gRPC reflection", target.authority),
            err,
        )
        .with_hint(
            "enable it on the server (register `tonic_reflection::server::Builder`), or \
             set `proto_files` in the active environment to load the schema from local \
             .proto files instead",
        )),
    }
}

/// One reflection conversation over `path`: list the services, then fetch the descriptor
/// file containing each. `ServerReflectionInfo` is bidirectional, so requests are fed in
/// through a channel while responses are read back off the same stream.
async fn reflect_over(
    channel: &tonic::transport::Channel,
    path: &'static str,
) -> Result<Schema, String> {
    use reflection::*;

    let (tx, rx) = tokio::sync::mpsc::channel::<ServerReflectionRequest>(16);
    let mut client = tonic::client::Grpc::new(channel.clone());
    client
        .ready()
        .await
        .map_err(|e| format!("service not ready: {e}"))?;

    let ask = |req: MessageRequest| ServerReflectionRequest {
        host: String::new(),
        message_request: Some(req),
    };

    // Prime the stream with ListServices before opening it — the server may not send
    // anything until it has a request, and the channel is buffered so this cannot block.
    tx.send(ask(MessageRequest::ListServices(String::new())))
        .await
        .map_err(|_| "reflection stream closed".to_string())?;

    let mut stream = client
        .streaming(
            Request::new(tokio_stream::wrappers::ReceiverStream::new(rx)),
            path.parse().map_err(|e| format!("bad path: {e}"))?,
            PbCodec::<ServerReflectionRequest, ServerReflectionResponse>::default(),
        )
        .await
        .map_err(|s| format!("reflection ({}): {}", s.code(), s.message()))?
        .into_inner();

    let services = match next_response(&mut stream).await? {
        MessageResponse::ListServices(list) => {
            list.service.into_iter().map(|s| s.name).collect::<Vec<_>>()
        }
        MessageResponse::Error(e) => {
            return Err(format!(
                "reflection error {}: {}",
                e.error_code, e.error_message
            ))
        }
        _ => return Err("reflection returned an unexpected response to ListServices".into()),
    };
    if services.is_empty() {
        return Err("the server exposes no gRPC services".into());
    }

    // One descriptor request per service; each response carries the defining file plus
    // its transitive dependencies, so the pool assembles from the union.
    let mut files: Vec<prost_types::FileDescriptorProto> = Vec::new();
    for service in &services {
        tx.send(ask(MessageRequest::FileContainingSymbol(service.clone())))
            .await
            .map_err(|_| "reflection stream closed".to_string())?;
        match next_response(&mut stream).await? {
            MessageResponse::FileDescriptor(fd) => {
                for bytes in fd.file_descriptor_proto {
                    let file = prost_types::FileDescriptorProto::decode(bytes.as_slice())
                        .map_err(|e| format!("bad descriptor for {service}: {e}"))?;
                    files.push(file);
                }
            }
            // A server may refuse an individual symbol (e.g. the reflection service
            // itself); skip it rather than failing the whole discovery.
            MessageResponse::Error(_) => continue,
            _ => continue,
        }
    }
    drop(tx);

    if files.is_empty() {
        return Err("reflection returned no descriptors".into());
    }
    let mut pool = DescriptorPool::new();
    // Order is arbitrary and duplicates are expected (shared dependencies come back once
    // per service); `add_file_descriptor_protos` resolves both.
    pool.add_file_descriptor_protos(files)
        .map_err(|e| format!("schema: {e}"))?;
    Ok(Schema::from_pool(pool))
}

/// Read the next reflection response off the stream.
async fn next_response(
    stream: &mut tonic::Streaming<reflection::ServerReflectionResponse>,
) -> Result<reflection::MessageResponse, String> {
    stream
        .message()
        .await
        .map_err(|s| format!("reflection ({}): {}", s.code(), s.message()))?
        .ok_or_else(|| "reflection stream ended early".to_string())?
        .message_response
        .ok_or_else(|| "empty reflection response".to_string())
}

/// Compile `.proto` files in-process with `protox` — the fallback when a server has
/// reflection disabled. No `protoc` binary is involved.
pub fn schema_from_protos(files: &[String], includes: &[String]) -> Result<Schema, GrpcError> {
    if files.is_empty() {
        return Err(GrpcError::new(
            "No .proto files configured",
            "no proto_files configured in the active environment",
        )
        .with_hint(
            "add `proto_files` to the active environment in \
             .moonlight/http/environments.json",
        ));
    }
    // protox needs somewhere to resolve imports from; default to each file's directory
    // so a single self-contained .proto works with no configuration at all.
    let mut includes: Vec<String> = includes.to_vec();
    if includes.is_empty() {
        for f in files {
            if let Some(parent) = Path::new(f).parent().and_then(|p| p.to_str()) {
                let parent = if parent.is_empty() { "." } else { parent };
                if !includes.iter().any(|i| i == parent) {
                    includes.push(parent.to_string());
                }
            }
        }
    }
    let set = protox::compile(files, includes).map_err(|e| {
        // protox reports the file, line and column, which is the whole value here.
        GrpcError::new("The .proto files did not compile", e.to_string()).with_hint(
            "an unresolved `import` usually means `proto_includes` is missing the \
             directory that import is relative to",
        )
    })?;
    let pool = DescriptorPool::from_file_descriptor_set(set)
        .map_err(|e| GrpcError::new("The compiled schema did not load", e.to_string()))?;
    Ok(Schema::from_pool(pool))
}

/// Where a loaded [`Schema`] came from — shown in the panel so the operator knows
/// whether they are looking at the live server's own view or a local file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaSource {
    Reflection,
    Protos,
}

impl SchemaSource {
    pub fn label(self) -> &'static str {
        match self {
            SchemaSource::Reflection => "server reflection",
            SchemaSource::Protos => "local .proto",
        }
    }
}

/// Load the schema for `target` under `env`: reflection first, local `.proto` second.
///
/// Reflection is preferred because it describes what the server *actually* serves; local
/// files can drift. When reflection is unavailable and no `proto_files` are configured,
/// the reflection error is returned unchanged — it already names both remedies.
pub async fn load_schema(
    target: &GrpcTarget,
    env: &http::HttpEnv,
) -> Result<(Schema, SchemaSource), GrpcError> {
    let reflection_err = match schema_from_reflection(target).await {
        Ok(schema) => return Ok((schema, SchemaSource::Reflection)),
        Err(e) => e,
    };
    if env.proto_files.is_empty() {
        return Err(reflection_err);
    }
    // Interpolate so proto paths can be written against the environment's variables.
    let files: Vec<String> = env
        .proto_files
        .iter()
        .map(|f| interpolate(f, &env.vars))
        .collect();
    let includes: Vec<String> = env
        .proto_includes
        .iter()
        .map(|i| interpolate(i, &env.vars))
        .collect();
    match schema_from_protos(&files, &includes) {
        Ok(schema) => Ok((schema, SchemaSource::Protos)),
        // Both routes failed. Lead with the .proto error — configuring `proto_files` is
        // a statement of intent that this is the route meant to work — but keep the
        // reflection failure, since either could be the one to fix.
        Err(proto_err) => Err(GrpcError {
            title: "Could not load a schema".into(),
            detail: format!(
                "{}\n\nreflection also failed: {}",
                proto_err.one_line(),
                reflection_err.one_line()
            ),
            hint: proto_err.hint,
        }),
    }
}

// ---------------------------------------------------------------------------
// Calling
// ---------------------------------------------------------------------------

/// A unary call to make: everything already resolved (target parsed, schema loaded,
/// variables interpolated).
pub struct CallSpec {
    pub target: GrpcTarget,
    /// `/pkg.Service/Method`.
    pub path: String,
    pub request: MessageDescriptor,
    pub response: MessageDescriptor,
    pub metadata: Vec<(String, String)>,
    /// The request message as JSON.
    pub message: String,
}

/// The outcome of a unary call. Mirrors [`http::HttpOutcome`]'s split between a compact
/// preview and the fuller panel body.
pub struct GrpcOutcome {
    /// The gRPC status code name, e.g. `OK`, `NOT_FOUND`. `None` on a transport failure
    /// that never reached a status.
    pub code: Option<String>,
    /// Numeric status code, for the `NOT_FOUND (5)` display.
    pub code_number: Option<i32>,
    pub status_message: String,
    pub ms: u64,
    pub ok: bool,
    pub bytes: usize,
    /// A failure that never produced a gRPC status — bad request JSON, an unreachable
    /// server, a rejected metadata key. A non-OK *status* is not one of these: it is a
    /// real answer, and lives in `code`/`status_message`.
    pub error: Option<GrpcError>,
    /// Response headers + trailers (panel only).
    pub metadata: Vec<(String, String)>,
    /// Fuller response JSON for the panel (≤ [`PANEL_BODY`]).
    pub body: String,
}

impl GrpcOutcome {
    fn failed(ms: u64, error: GrpcError) -> Self {
        Self {
            code: None,
            code_number: None,
            status_message: String::new(),
            ms,
            ok: false,
            bytes: 0,
            error: Some(error),
            metadata: Vec::new(),
            body: String::new(),
        }
    }
}

/// Perform one unary call. Never panics; every failure surfaces as a [`GrpcOutcome`] so
/// the panel renders errors the same way it renders responses.
pub async fn call(spec: CallSpec) -> GrpcOutcome {
    let started = Instant::now();
    let elapsed = |t: Instant| t.elapsed().as_millis() as u64;

    // JSON -> DynamicMessage, validated against the request descriptor. A typo in a field
    // name fails here, before any connection is made.
    let mut de = serde_json::Deserializer::from_str(spec.message.trim());
    let request = match DynamicMessage::deserialize(spec.request.clone(), &mut de) {
        Ok(m) => m,
        Err(e) => {
            return GrpcOutcome::failed(
                elapsed(started),
                GrpcError::new(
                    format!("The request is not a valid {}", spec.request.full_name()),
                    e.to_string(),
                )
                .with_hint(
                    "the Form tab builds this message from the server's own descriptor, \
                     so every field is guaranteed to fit",
                ),
            )
        }
    };

    let channel = match connect(&spec.target).await {
        Ok(c) => c,
        Err(e) => return GrpcOutcome::failed(elapsed(started), e),
    };

    let mut req = Request::new(request);
    for (k, v) in &spec.metadata {
        // Metadata keys are lowercase ASCII by protocol; a bad key is the operator's
        // typo, so it is reported rather than silently dropped.
        let key = match k
            .to_ascii_lowercase()
            .parse::<tonic::metadata::MetadataKey<_>>()
        {
            Ok(k) => k,
            Err(_) => {
                return GrpcOutcome::failed(
                    elapsed(started),
                    GrpcError::new(
                        "Invalid metadata key",
                        format!("`{k}` is not usable as a key"),
                    )
                    .with_hint(
                        "metadata keys are lowercase letters, digits, `-`, `_` and `.` \
                             only — no spaces or colons",
                    ),
                )
            }
        };
        let val = match v.parse::<tonic::metadata::MetadataValue<_>>() {
            Ok(v) => v,
            Err(_) => {
                return GrpcOutcome::failed(
                    elapsed(started),
                    GrpcError::new(
                        format!("Invalid metadata value for `{k}`"),
                        "the value contains characters that are not printable ASCII",
                    )
                    .with_hint(
                        "an unresolved `{{var}}` is the usual cause — check the active \
                         environment defines it",
                    ),
                )
            }
        };
        req.metadata_mut().insert(key, val);
    }

    let path = match spec.path.parse() {
        Ok(p) => p,
        Err(e) => {
            return GrpcOutcome::failed(
                elapsed(started),
                GrpcError::new(
                    format!("Invalid method path `{}`", spec.path),
                    format!("{e}"),
                ),
            )
        }
    };

    let mut client = tonic::client::Grpc::new(channel);
    if let Err(e) = client.ready().await {
        return GrpcOutcome::failed(
            elapsed(started),
            GrpcError::new(
                format!(
                    "{} accepted the connection but is not serving",
                    spec.target.authority
                ),
                error_chain(&e),
            ),
        );
    }

    let codec = DynamicCodec::new(spec.response.clone());
    let result = client.unary(req, path, codec).await;
    let ms = elapsed(started);

    match result {
        Ok(response) => {
            let metadata = metadata_pairs(response.metadata());
            let body = serialize_response(response.into_inner());
            GrpcOutcome {
                code: Some("OK".into()),
                code_number: Some(0),
                status_message: String::new(),
                ms,
                ok: true,
                bytes: body.len(),
                error: None,
                metadata,
                body: clip(&body, PANEL_BODY),
            }
        }
        // A non-OK status is a real gRPC answer, not a transport failure — surface the
        // code and any error details the same way a non-2xx HTTP status is surfaced.
        Err(status) => {
            let metadata = metadata_pairs(status.metadata());
            GrpcOutcome {
                code: Some(format!("{:?}", status.code()).to_uppercase()),
                code_number: Some(status.code() as i32),
                status_message: status.message().to_string(),
                ms,
                ok: false,
                bytes: 0,
                error: None,
                metadata,
                body: String::new(),
            }
        }
    }
}

/// Render a response message as pretty protobuf-JSON.
fn serialize_response(msg: DynamicMessage) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::pretty(&mut buf);
    // `stringify_64_bit_integers` off keeps int64 as JSON numbers, which reads better in
    // the panel than the spec's string form.
    let opts = SerializeOptions::new()
        .stringify_64_bit_integers(false)
        .skip_default_fields(false);
    match msg.serialize_with_options(&mut ser, &opts) {
        Ok(()) => String::from_utf8(buf).unwrap_or_default(),
        Err(e) => format!("<response could not be rendered as JSON: {e}>"),
    }
}

/// Flatten response metadata (headers and trailers) into display pairs.
///
/// Binary entries are listed by key with a `<binary>` marker rather than dropped: the key
/// alone is informative — `grpc-status-details-bin` present on a failure tells the
/// operator the server sent structured error details, even though this pane can't decode
/// them.
fn metadata_pairs(map: &tonic::metadata::MetadataMap) -> Vec<(String, String)> {
    map.iter()
        .map(|kv| match kv {
            tonic::metadata::KeyAndValueRef::Ascii(k, v) => (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            ),
            tonic::metadata::KeyAndValueRef::Binary(k, _) => {
                (k.as_str().to_string(), "<binary>".to_string())
            }
        })
        .collect()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// A tokio runtime for gRPC work, mirroring
/// [`McpHostHandle::build`](crate::views::mcp_host::McpHostHandle::build).
///
/// tonic is built on hyper, which needs tokio's IO driver; GPUI's background executor is
/// not one, so gRPC cannot simply run there the way the HTTP tool's blocking `ureq` call
/// does. This owns the reactor those calls need.
///
/// **Callers must not block on it.** [`spawn`](Self::spawn) hands back a
/// `JoinHandle`, which is pollable from GPUI's executor — so a panel awaits it inside
/// `cx.spawn` and neither side is blocked. A `block_on` here would freeze the UI thread
/// for the duration of a call, which for a hung server is the full timeout.
#[derive(Clone)]
pub struct GrpcRuntime {
    handle: tokio::runtime::Handle,
}

impl GrpcRuntime {
    /// Stand the runtime up. `None` (logged) when it can't be built — the gRPC panel then
    /// reports that rather than the app failing to start.
    pub fn build() -> Option<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("moonlight-grpc")
            .enable_all()
            .build()
            .map_err(|err| tracing::error!(error = %err, "gRPC runtime failed to build"))
            .ok()?;
        let handle = rt.handle().clone();
        // The runtime must outlive every in-flight call — kept for the app's lifetime,
        // exactly as the MCP host's is.
        Box::leak(Box::new(rt));
        Some(Self { handle })
    }

    /// Run `fut` on the gRPC runtime; await the result from any executor.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(fut)
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// One completed (or failed) gRPC call, as the history shows it. Like [`http::HttpCall`]
/// it deliberately omits request metadata so a bearer token never lands in the ring.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GrpcCall {
    /// `pkg.Service/Method`.
    pub method: String,
    pub target: String,
    /// Status code name, `None` on a transport failure.
    pub code: Option<String>,
    pub ms: u64,
    pub ok: bool,
    pub bytes: usize,
    pub at_millis: i64,
    pub error: Option<String>,
}

struct Inner {
    calls: std::collections::VecDeque<GrpcCall>,
}

/// Thread-safe handle to the gRPC call history; cheap to clone, every clone sees the
/// same ring.
#[derive(Clone)]
pub struct GrpcHistory {
    inner: Arc<Mutex<Inner>>,
}

impl Default for GrpcHistory {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                calls: std::collections::VecDeque::new(),
            })),
        }
    }
}

impl GrpcHistory {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn record(&self, call: GrpcCall) {
        let mut inner = self.lock();
        inner.calls.push_back(call);
        while inner.calls.len() > HISTORY_CAP {
            inner.calls.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.lock().calls.len()
    }

    /// Kept alongside `len` (clippy::len_without_is_empty); used by tests today.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `n` most recent calls, newest first.
    pub fn recent(&self, n: usize) -> Vec<GrpcCall> {
        self.lock().calls.iter().rev().take(n).cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Saved requests
// ---------------------------------------------------------------------------

/// A gRPC call the operator saved for reuse.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedGrpcRequest {
    pub name: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub metadata: Vec<(String, String)>,
    #[serde(default)]
    pub message: String,
}

/// `.moonlight/grpc/requests.json`: the operator's saved gRPC calls.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GrpcRequests {
    #[serde(default)]
    pub requests: Vec<SavedGrpcRequest>,
}

fn grpc_dir(root: &Path) -> std::path::PathBuf {
    root.join(".moonlight").join("grpc")
}

/// Load the saved gRPC requests under `root` (missing/bad file → empty).
pub fn load_requests(root: &Path) -> GrpcRequests {
    std::fs::read_to_string(grpc_dir(root).join("requests.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the saved gRPC requests.
pub fn save_requests(root: &Path, reqs: &GrpcRequests) -> std::io::Result<()> {
    let dir = grpc_dir(root);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(reqs).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("requests.json"), text)
}

/// Whether a gRPC target is reachable under the active environment's host scope.
///
/// Delegates to the HTTP tool's gate unchanged: `host_of` already yields `localhost` for
/// a bare `localhost:50051`, so both tools enforce one rule from one manifest.
pub fn target_allowed(target: &GrpcTarget, allowed_hosts: &[String]) -> bool {
    http::host_allowed(&target.authority, allowed_hosts)
}

/// Interpolate `{{vars}}` in a call's editable fields, reusing the HTTP tool's syntax so
/// one environment serves both tools.
pub fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> String {
    http::interpolate(template, vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_accepts_bare_authority_and_schemes() {
        assert_eq!(
            parse_target("localhost:50051").unwrap(),
            GrpcTarget {
                authority: "localhost:50051".into(),
                tls: false
            }
        );
        assert_eq!(
            parse_target("grpc://10.0.0.5:9000").unwrap(),
            GrpcTarget {
                authority: "10.0.0.5:9000".into(),
                tls: false
            }
        );
        // An explicit TLS scheme implies the standard port.
        assert_eq!(
            parse_target("https://api.example.com").unwrap(),
            GrpcTarget {
                authority: "api.example.com:443".into(),
                tls: true
            }
        );
        assert_eq!(
            parse_target("grpcs://api.example.com:8443").unwrap(),
            GrpcTarget {
                authority: "api.example.com:8443".into(),
                tls: true
            }
        );
        // A pasted path is trimmed off — a gRPC target is an authority only.
        assert_eq!(
            parse_target("http://localhost:50051/some/path")
                .unwrap()
                .authority,
            "localhost:50051"
        );
    }

    #[test]
    fn parse_target_rejects_what_it_cannot_guess() {
        // No default gRPC port exists, so a bare host is an error, not a guess.
        let err = parse_target("localhost").unwrap_err();
        assert!(err.contains("needs a port"), "{err}");
        assert!(parse_target("").is_err());
        assert!(parse_target("ftp://host:1").is_err());
    }

    #[test]
    fn endpoint_url_maps_grpc_schemes_onto_http() {
        let plain = parse_target("localhost:50051").unwrap();
        assert_eq!(plain.endpoint_url(), "http://localhost:50051");
        let tls = parse_target("grpcs://api.example.com:443").unwrap();
        assert_eq!(tls.endpoint_url(), "https://api.example.com:443");
    }

    #[test]
    fn target_allowed_reuses_the_http_host_gate() {
        let local = parse_target("localhost:50051").unwrap();
        let public = parse_target("grpcs://api.example.com:443").unwrap();
        // Empty allowlist → loopback/private only, exactly as for HTTP.
        assert!(target_allowed(&local, &[]));
        assert!(!target_allowed(&public, &[]));
        // An explicit allowlist widens it, dot-suffix included.
        let allow = vec!["example.com".to_string()];
        assert!(target_allowed(&public, &allow));
        assert!(target_allowed(
            &parse_target("grpcs://api.sub.example.com:443").unwrap(),
            &allow
        ));
        assert!(!target_allowed(
            &parse_target("grpcs://evil-example.com:443").unwrap(),
            &allow
        ));
    }

    /// A self-contained schema compiled the same way the .proto fallback compiles one.
    ///
    /// `tag` keeps each test in its own directory: the test binary runs them in parallel
    /// threads of one process, so a process-id-only path would let one test delete the
    /// fixture another is still reading.
    fn test_schema(tag: &str) -> (Schema, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ml-grpc-proto-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.proto");
        std::fs::write(
            &path,
            r#"
syntax = "proto3";
package testpkg;

enum Role { ROLE_UNKNOWN = 0; ROLE_ADMIN = 1; }

message GetUserRequest {
  int32 id = 1;
  string email = 2;
  bool active = 3;
  repeated string tags = 4;
  Role role = 5;
  map<string, string> labels = 6;
}
message GetUserResponse { string name = 1; int64 seen_at = 2; }

service UserService {
  rpc GetUser(GetUserRequest) returns (GetUserResponse);
  rpc Watch(GetUserRequest) returns (stream GetUserResponse);
  rpc Upload(stream GetUserRequest) returns (GetUserResponse);
  rpc Chat(stream GetUserRequest) returns (stream GetUserResponse);
}
"#,
        )
        .unwrap();
        let schema = schema_from_protos(
            &[path.to_string_lossy().to_string()],
            &[dir.to_string_lossy().to_string()],
        )
        .unwrap();
        (schema, dir)
    }

    #[test]
    fn protos_compile_without_protoc_and_expose_services() {
        let (schema, dir) = test_schema("services");
        assert_eq!(schema.services(), vec!["testpkg.UserService".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn methods_flag_streaming_so_the_ui_can_disable_them() {
        let (schema, dir) = test_schema("streaming");
        let methods = schema.methods("testpkg.UserService");
        let by = |n: &str| methods.iter().find(|m| m.name == n).unwrap().clone();

        assert!(by("GetUser").is_unary());
        assert_eq!(by("GetUser").path, "/testpkg.UserService/GetUser");
        assert_eq!(by("Watch").streaming, Some("server streaming"));
        assert_eq!(by("Upload").streaming, Some("client streaming"));
        assert_eq!(by("Chat").streaming, Some("bidirectional streaming"));
        assert!(!by("Chat").is_unary());

        // An unknown service yields nothing rather than panicking.
        assert!(schema.methods("nope.Missing").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_round_trips_through_a_dynamic_message() {
        let (schema, dir) = test_schema("roundtrip");
        let (req, _resp) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();

        // JSON in -> DynamicMessage -> protobuf bytes.
        let json = r#"{"id":42,"email":"ada@example.com","active":true,"tags":["a","b"],"role":"ROLE_ADMIN"}"#;
        let mut de = serde_json::Deserializer::from_str(json);
        let msg = DynamicMessage::deserialize(req.clone(), &mut de).unwrap();
        let bytes = msg.encode_to_vec();
        assert!(!bytes.is_empty());

        // bytes -> DynamicMessage -> JSON out, values preserved.
        let back = DynamicMessage::decode(req, bytes.as_slice()).unwrap();
        let out = serialize_response(back);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], serde_json::json!(42));
        assert_eq!(v["email"], serde_json::json!("ada@example.com"));
        assert_eq!(v["active"], serde_json::json!(true));
        assert_eq!(v["tags"], serde_json::json!(["a", "b"]));
        assert_eq!(v["role"], serde_json::json!("ROLE_ADMIN"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_field_name_is_rejected_before_any_connection() {
        let (schema, dir) = test_schema("badfield");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let mut de = serde_json::Deserializer::from_str(r#"{"nonsense": 1}"#);
        assert!(DynamicMessage::deserialize(req, &mut de).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn example_json_covers_every_field_shape() {
        let (schema, dir) = test_schema("example");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let example = schema.example_json(&req);
        let v: serde_json::Value = serde_json::from_str(&example).unwrap();

        assert_eq!(v["id"], serde_json::json!(0));
        assert_eq!(v["email"], serde_json::json!(""));
        assert_eq!(v["active"], serde_json::json!(false));
        // Repeated fields get a one-element sample; maps get an empty object.
        assert_eq!(v["tags"], serde_json::json!([""]));
        assert_eq!(v["labels"], serde_json::json!({}));
        // Enums use the variant name, per protobuf JSON.
        assert_eq!(v["role"], serde_json::json!("ROLE_UNKNOWN"));

        // The skeleton must itself be a valid request.
        let mut de = serde_json::Deserializer::from_str(&example);
        assert!(DynamicMessage::deserialize(req, &mut de).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The codec's substance is that a response can be decoded with **no type known at
    /// compile time** — only a descriptor. This exercises exactly the encode/merge pair
    /// `DynEncoder`/`DynDecoder` perform.
    ///
    /// It stops short of driving them through `tonic::codec::{EncodeBuf, DecodeBuf}`:
    /// those have no public constructor (tonic builds them internally), so the thin
    /// buffer wrapper around this logic is only covered when a real call runs.
    #[test]
    fn a_message_round_trips_knowing_only_its_descriptor() {
        let (schema, dir) = test_schema("descriptor");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();

        let mut msg = DynamicMessage::new(req.clone());
        msg.set_field_by_name("id", prost_reflect::Value::I32(7));
        let bytes = msg.encode_to_vec();

        // What `DynDecoder` does: build from the descriptor it carries, then merge.
        let mut decoded = DynamicMessage::new(req.clone());
        decoded.merge(bytes.as_slice()).unwrap();
        assert_eq!(decoded.get_field_by_name("id").unwrap().as_i32(), Some(7));

        // And the codec hands out an encoder/decoder pair for that descriptor.
        let mut codec = DynamicCodec::new(req);
        let _ = codec.encoder();
        let _ = codec.decoder();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schema_from_protos_reports_a_missing_configuration() {
        let err = schema_from_protos(&[], &[]).unwrap_err();
        assert!(err.detail.contains("no proto_files"), "{err:?}");
        // A configuration error is only useful if it names where the configuration lives.
        assert!(
            err.hint.unwrap().contains("environments.json"),
            "the hint must name the file to edit"
        );
    }

    /// Adding the gRPC fields must not disturb manifests written before they existed —
    /// every `.moonlight/http/environments.json` already on disk predates them.
    #[test]
    fn manifests_without_the_grpc_fields_still_load() {
        let json = r#"{
            "active": "local",
            "environments": {
                "local": {
                    "vars": { "base_url": "http://localhost:8080" },
                    "allowed_hosts": ["example.com"]
                }
            }
        }"#;
        let manifest: http::HttpManifest = serde_json::from_str(json).unwrap();
        let env = manifest.active_env();
        assert_eq!(env.vars.get("base_url").unwrap(), "http://localhost:8080");
        assert_eq!(env.allowed_hosts, vec!["example.com".to_string()]);
        // The new fields default to empty — i.e. reflection-only, no .proto configured.
        assert!(env.proto_files.is_empty());
        assert!(env.proto_includes.is_empty());
    }

    /// With no `proto_files` configured there is nothing to fall back to, so the
    /// reflection failure must surface unchanged rather than being masked.
    #[test]
    fn load_schema_without_protos_reports_the_reflection_failure() {
        let target = parse_target("127.0.0.1:1").unwrap();
        let env = http::HttpEnv::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(load_schema(&target, &env)).unwrap_err();
        // Connection to a closed port fails before any reflection dialogue, and the
        // failure must be about *reaching* the server, not about reflection.
        assert_eq!(err.title, "Could not connect to 127.0.0.1:1");
        assert!(
            err.hint.unwrap().contains("check the port"),
            "{:?}",
            err.detail
        );
    }

    /// When reflection is unreachable but `.proto` files are configured, the schema still
    /// loads — the whole point of the fallback.
    #[test]
    fn load_schema_falls_back_to_configured_protos() {
        let (_schema, dir) = test_schema("fallback");
        let env = http::HttpEnv {
            proto_files: vec![dir.join("test.proto").to_string_lossy().to_string()],
            proto_includes: vec![dir.to_string_lossy().to_string()],
            ..Default::default()
        };
        // Port 1 is closed, so reflection cannot succeed.
        let target = parse_target("127.0.0.1:1").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (schema, source) = rt.block_on(load_schema(&target, &env)).unwrap();
        assert_eq!(source, SchemaSource::Protos);
        assert_eq!(schema.services(), vec!["testpkg.UserService".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The form's whole claim is that the descriptor already knows the shape — so every
    /// field must arrive with the right *kind* of cell, not merely be present.
    #[test]
    fn form_fields_give_every_field_the_right_cell() {
        let (schema, dir) = test_schema("formfields");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&req);
        let by = |p: &str| fields.iter().find(|f| f.path == p).unwrap().clone();

        assert_eq!(by("id").input, FormInput::Text { numeric: true });
        assert_eq!(by("email").input, FormInput::Text { numeric: false });
        assert_eq!(by("active").input, FormInput::Bool);
        assert_eq!(
            by("role").input,
            FormInput::Enum(vec!["ROLE_UNKNOWN".into(), "ROLE_ADMIN".into()])
        );
        // Repeated and map fields keep a JSON cell; the placeholder shows the shape.
        assert_eq!(
            by("tags").input,
            FormInput::Json {
                hint: r#"["…"]"#.into()
            }
        );
        assert_eq!(
            by("labels").input,
            FormInput::Json {
                hint: r#"{"key": "value"}"#.into()
            }
        );
        // The type column reads as the .proto declared it.
        assert_eq!(by("tags").type_label, "repeated string");
        assert_eq!(by("labels").type_label, "map<string, string>");
        assert_eq!(by("role").type_label, "testpkg.Role");

        // A snake_case field carries both spellings, so either can be read back.
        let seen = fields.iter().find(|f| f.name == "seenAt");
        assert!(
            seen.is_none(),
            "seen_at belongs to the response, not the request"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An untouched form must produce `{}` — not a message of zeros. In proto3 a zero and
    /// an unset field look identical on the wire, but sending zeros states things the
    /// operator never said.
    #[test]
    fn empty_cells_are_omitted_not_zeroed() {
        let (schema, dir) = test_schema("formempty");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&req);

        let mut values = BTreeMap::new();
        assert_eq!(build_message_json(&fields, &values), "{}");
        // Whitespace is not a value either.
        values.insert("email".to_string(), "   ".to_string());
        assert_eq!(build_message_json(&fields, &values), "{}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What the table builds must be exactly what the wire accepts — the form is only
    /// worth having if its output needs no hand-editing.
    #[test]
    fn the_form_builds_a_message_the_descriptor_accepts() {
        let (schema, dir) = test_schema("formbuild");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&req);

        let values: BTreeMap<String, String> = [
            ("id", "42"),
            ("email", "ada@example.com"),
            ("active", "true"),
            ("role", "ROLE_ADMIN"),
            ("tags", r#"["a","b"]"#),
            ("labels", r#"{"team":"core"}"#),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let json = build_message_json(&fields, &values);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Typed cells become typed JSON, not strings.
        assert_eq!(v["id"], serde_json::json!(42));
        assert_eq!(v["active"], serde_json::json!(true));
        assert_eq!(v["role"], serde_json::json!("ROLE_ADMIN"));
        assert_eq!(v["tags"], serde_json::json!(["a", "b"]));
        assert_eq!(v["labels"], serde_json::json!({ "team": "core" }));

        let mut de = serde_json::Deserializer::from_str(&json);
        assert!(DynamicMessage::deserialize(req, &mut de).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A number cell holding `{{var}}` must survive to interpolation rather than being
    /// dropped for not parsing as a number.
    #[test]
    fn a_variable_in_a_number_cell_is_kept_as_text() {
        let (schema, dir) = test_schema("formvar");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&req);
        let values = BTreeMap::from([("id".to_string(), "{{user_id}}".to_string())]);

        let json = build_message_json(&fields, &values);
        assert!(json.contains("{{user_id}}"), "{json}");
        // And after interpolation it is a valid request.
        let vars = BTreeMap::from([("user_id".to_string(), "7".to_string())]);
        let filled = interpolate(&json, &vars);
        let mut de = serde_json::Deserializer::from_str(&filled);
        let msg = DynamicMessage::deserialize(req, &mut de).unwrap();
        assert_eq!(msg.get_field_by_name("id").unwrap().as_i32(), Some(7));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Switching Form → JSON → Form must not quietly lose what was typed, including
    /// fields a person wrote in the .proto's own snake_case rather than protobuf-JSON's
    /// camelCase.
    #[test]
    fn form_values_round_trip_through_json_in_either_spelling() {
        let (schema, dir) = test_schema("formseed");
        let (_, resp) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&resp);
        assert!(fields.iter().any(|f| f.path == "seenAt"));

        // Authored the protobuf-JSON way.
        let camel = seed_form_values(&fields, r#"{"name":"ada","seenAt":99}"#);
        assert_eq!(camel.get("name").unwrap(), "ada");
        assert_eq!(camel.get("seenAt").unwrap(), "99");

        // Authored the .proto way — same cells.
        let snake = seed_form_values(&fields, r#"{"name":"ada","seen_at":99}"#);
        assert_eq!(snake, camel);

        // And a full round trip is stable.
        let rebuilt = build_message_json(&fields, &camel);
        assert_eq!(seed_form_values(&fields, &rebuilt), camel);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The example skeleton is what seeds the form, so it has to land in the cells.
    #[test]
    fn the_example_skeleton_seeds_the_form() {
        let (schema, dir) = test_schema("formexample");
        let (req, _) = schema
            .method_types("testpkg.UserService", "GetUser")
            .unwrap();
        let fields = schema.form_fields(&req);
        let seeded = seed_form_values(&fields, &schema.example_json(&req));

        assert_eq!(seeded.get("id").unwrap(), "0");
        assert_eq!(seeded.get("active").unwrap(), "false");
        assert_eq!(seeded.get("role").unwrap(), "ROLE_UNKNOWN");
        assert_eq!(seeded.get("tags").unwrap(), r#"[""]"#);
        // `email`'s example is the empty string, which is indistinguishable from an
        // untouched cell — so it stays unset rather than being sent as "".
        assert!(!seeded.contains_key("email"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Nested messages flatten into indented rows under a group header, and stop before
    /// a self-referential type could flatten forever.
    #[test]
    fn nested_messages_flatten_into_indented_rows() {
        let dir = std::env::temp_dir().join(format!("ml-grpc-nest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nest.proto");
        std::fs::write(
            &path,
            r#"
syntax = "proto3";
package nest;

message Node { string label = 1; Node child = 2; }
message Profile { string bio = 1; }
message Req { string id = 1; Profile profile = 2; Node tree = 3; }
message Res { string ok = 1; }
service S { rpc Call(Req) returns (Res); }
"#,
        )
        .unwrap();
        let schema = schema_from_protos(
            &[path.to_string_lossy().to_string()],
            &[dir.to_string_lossy().to_string()],
        )
        .unwrap();
        let (req, _) = schema.method_types("nest.S", "Call").unwrap();
        let fields = schema.form_fields(&req);
        let by = |p: &str| fields.iter().find(|f| f.path == p).unwrap().clone();

        // The message itself is a header; its field is an indented row beneath.
        assert_eq!(by("profile").input, FormInput::Group);
        assert_eq!(by("profile").depth, 0);
        assert_eq!(by("profile.bio").depth, 1);
        assert!(by("profile.bio").is_editable());
        assert!(!by("profile").is_editable());

        // The self-referential branch stops at MAX_FORM_DEPTH and hands the rest to a
        // JSON cell rather than recursing.
        assert!(fields.iter().all(|f| f.depth <= MAX_FORM_DEPTH));
        let deepest = fields
            .iter()
            .filter(|f| f.path.starts_with("tree"))
            .max_by_key(|f| f.depth)
            .unwrap();
        assert!(matches!(deepest.input, FormInput::Json { .. }));

        // A nested value nests in the built JSON.
        let values = BTreeMap::from([("profile.bio".to_string(), "hi".to_string())]);
        let json = build_message_json(&fields, &values);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["profile"]["bio"], serde_json::json!("hi"));
        // And an untouched group contributes nothing at all.
        assert!(v.as_object().unwrap().get("tree").is_none());

        let mut de = serde_json::Deserializer::from_str(&json);
        assert!(DynamicMessage::deserialize(req, &mut de).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `transport error` on its own is unactionable; the cause chain is where every
    /// useful fact lives.
    #[test]
    fn error_chain_unwraps_the_causes() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection refused")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "transport error")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        assert_eq!(
            error_chain(&Outer(Inner)),
            "transport error → connection refused"
        );
    }

    /// Click-to-copy is only useful if what lands on the clipboard is pasteable as-is —
    /// a value still wearing its quotes and comma has to be hand-edited, which is the
    /// work the affordance exists to remove.
    #[test]
    fn a_response_line_copies_as_a_pasteable_value() {
        assert_eq!(json_line_value(r#"  "id": 42,"#), "42");
        assert_eq!(
            json_line_value(r#"  "email": "ada@example.com","#),
            "ada@example.com"
        );
        assert_eq!(json_line_value(r#"  "active": true"#), "true");
        // The last line of an object has no comma.
        assert_eq!(json_line_value(r#"  "name": "ada""#), "ada");
        // Array elements carry no key.
        assert_eq!(json_line_value(r#"    "core","#), "core");
        // Escapes are resolved, so what is copied is the value itself.
        assert_eq!(json_line_value(r#"  "bio": "a\nb","#), "a\nb");
        // Structural lines have no value of their own and copy as they read.
        assert_eq!(json_line_value(r#"  "profile": {"#), r#""profile": {"#);
        assert_eq!(json_line_value(r#"  "tags": ["#), r#""tags": ["#);
        assert_eq!(json_line_value("}"), "}");
    }

    #[test]
    fn an_error_copies_with_all_three_parts() {
        let err =
            GrpcError::new("Could not connect", "connection refused").with_hint("check the port");
        assert_eq!(
            err.as_block(),
            "Could not connect\nconnection refused\n→ check the port"
        );
        // The history list gets the flat form instead.
        assert_eq!(err.one_line(), "Could not connect: connection refused");
        // A hintless, detailless error degrades to just its title rather than to
        // stray punctuation.
        assert_eq!(GrpcError::new("Gone", "").as_block(), "Gone");
    }

    #[test]
    fn a_status_code_carries_an_actionable_hint() {
        // The codes an operator actually hits get a local explanation...
        assert!(status_hint(tonic::Code::Unimplemented)
            .unwrap()
            .contains("method names"));
        assert!(status_hint(tonic::Code::Unauthenticated)
            .unwrap()
            .contains("authorization"));
        // ...and the ones with nothing useful to add stay quiet rather than padding.
        assert!(status_hint(tonic::Code::Ok).is_none());
        assert!(status_hint(tonic::Code::Internal).is_none());
    }

    #[test]
    fn history_rings_and_reports_recent_newest_first() {
        let h = GrpcHistory::new();
        assert!(h.is_empty());
        for i in 0..(HISTORY_CAP + 5) {
            h.record(GrpcCall {
                method: format!("pkg.Svc/M{i}"),
                target: "localhost:50051".into(),
                code: Some("OK".into()),
                ms: 1,
                ok: true,
                bytes: 0,
                at_millis: i as i64,
                error: None,
            });
        }
        assert_eq!(h.len(), HISTORY_CAP);
        let recent = h.recent(3);
        assert_eq!(recent[0].method, format!("pkg.Svc/M{}", HISTORY_CAP + 4));
        assert_eq!(recent[2].method, format!("pkg.Svc/M{}", HISTORY_CAP + 2));
    }

    #[test]
    fn saved_requests_round_trip_on_disk() {
        let root = std::env::temp_dir().join(format!("ml-grpc-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(load_requests(&root).requests.is_empty());

        let reqs = GrpcRequests {
            requests: vec![SavedGrpcRequest {
                name: "UserService/GetUser".into(),
                target: "{{grpc_host}}".into(),
                service: "testpkg.UserService".into(),
                method: "GetUser".into(),
                metadata: vec![("authorization".into(), "Bearer {{token}}".into())],
                message: "{\"id\": 42}".into(),
            }],
        };
        save_requests(&root, &reqs).unwrap();
        let back = load_requests(&root);
        assert_eq!(back.requests.len(), 1);
        assert_eq!(back.requests[0].service, "testpkg.UserService");
        assert_eq!(back.requests[0].target, "{{grpc_host}}");
        assert_eq!(
            back.requests[0].metadata,
            vec![("authorization".to_string(), "Bearer {{token}}".to_string())]
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
