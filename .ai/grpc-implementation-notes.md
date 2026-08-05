# gRPC tool — verified implementation facts

Facts confirmed against docs.rs / upstream sources before implementation, so they
don't get re-derived or guessed at later.

## Versions that matter

| Crate | Version | Note |
|---|---|---|
| tonic | 0.14.6 | Sits on hyper 1.x / h2 0.4 / tower 0.5 — all already in `Cargo.lock` |
| prost-reflect | 0.16.5 | Depends on **prost 0.14** — same major as tonic 0.14, no dual-prost split |
| protox | 0.9.1 | Pure-Rust protoc; depends on prost-reflect ^0.16, re-exports it |
| tonic-reflection | 0.14.6 | **Server-only — has no `client` feature** |

`prost-reflect` ships **zero default features**; `serde` must be named explicitly
(that's what provides JSON in both directions).

tonic features needed: `channel`, `tls-ring`, `tls-native-roots`.
NOT needed: `codegen`, `router`, `server`, `tonic-prost` (we write our own codec).

## `tonic::codec::Codec` — exact signature (tonic 0.14.6)

```rust
pub trait Codec {
    type Encode: Send + 'static;
    type Decode: Send + 'static;
    type Encoder: Encoder<Item = Self::Encode, Error = Status> + Send + 'static;
    type Decoder: Decoder<Item = Self::Decode, Error = Status> + Send + 'static;
    fn encoder(&mut self) -> Self::Encoder;
    fn decoder(&mut self) -> Self::Decoder;
}

pub trait Encoder {
    type Item;
    type Error: From<io::Error>;
    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error>;
}

pub trait Decoder {
    type Item;
    type Error: From<io::Error>;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error>;
}
```

Note the `Error = Status` constraint on the associated encoder/decoder — our impls
must use `Status`, not a custom error. `Status: From<io::Error>` satisfies the bound.

`DynamicCodec` shape: the **decoder must carry the response `MessageDescriptor`**,
because `DynamicMessage` has no `Default` — it can only be built from a descriptor.
That is precisely why the stock prost codec cannot be used here.

```rust
// encode: item.encode(dst)                      (DynamicMessage: prost::Message)
// decode: DynamicMessage::new(desc.clone()) then .merge(src) -> Ok(Some(msg))
```

## `tonic::client::Grpc`

```rust
pub async fn unary<M1, M2, C>(&mut self, request: Request<M1>, path: PathAndQuery, codec: C)
    -> Result<Response<M2>, Status>
where C: Codec<Encode = M1, Decode = M2>
```

Path format: `/pkg.Service/Method`.

## prost-reflect API

```rust
DynamicMessage::new(desc: MessageDescriptor) -> Self
DynamicMessage::decode(desc: MessageDescriptor, buf: impl Buf) -> Result<Self, DecodeError>
DynamicMessage::deserialize(desc, deserializer) -> Result<Self, D::Error>   // JSON in
msg.serialize_with_options(serializer, &SerializeOptions)                   // JSON out
```

`DynamicMessage` implements `prost::Message`.

## Server reflection — must be hand-rolled

`tonic-reflection` has no client. Rather than add `tonic-build` + a vendored
`.proto` + a `build.rs`, declare the used subset as `#[derive(prost::Message)]`
structs (~60 lines): `ServerReflectionRequest`/`Response`, `FileDescriptorResponse`,
`ListServiceResponse`, `ServiceResponse`, `ErrorResponse`.

`ServerReflectionInfo` is **bidi-streaming**, so the reflection path uses
`Grpc::streaming` even though target calls are unary-only. It is the only streaming
code path in this increment.

## Request form (added after the first increment)

The Message tab defaults to **Form**, not raw JSON: reflection already knows every field
and type, so `Schema::form_fields` flattens the request descriptor into `FormField` rows
(dotted `path`, `depth`, `type_label`, `FormInput`) and the panel renders one row each.
JSON stays as a second mode; the two convert on switch.

Constraints that shaped it:

- **Every cell defaults to `null` = omitted**, not zeroed. `build_message_json` skips
  empty cells entirely, because in proto3 a sent zero and an unset field are
  indistinguishable on the wire but *not* to a server that reads `""` as "clear this".
- `FormField` carries **both** `path` (protobuf-JSON camelCase) and `alt_path` (the
  `.proto`'s own snake_case). Protobuf JSON accepts either spelling, so reading a message
  back into the form has to try both or it silently drops hand-written snake_case fields
  on a mode switch.
- Repeated fields and maps keep a raw-JSON cell. Check `is_map()` **before** `is_list()`:
  a map field is also repeated, and the wrong order reads `map<k,v>` as a list.
- Nested messages flatten to `MAX_FORM_DEPTH` (3) and then fall back to a JSON cell —
  protobuf allows self-referential types, which would otherwise flatten forever.
- A numeric cell holding `{{var}}` is emitted as a JSON *string* rather than dropped for
  not parsing as a number; interpolation runs on the serialized message afterwards, and
  quoted numbers are the spec's own form for 64-bit ints anyway.
- JSON → Form is refused when the text doesn't parse, rather than seeding an empty table
  from it — silently blanking a message the operator typed is the worst failure here.

## Error presentation

`GrpcError { title, detail, hint }` replaces error strings everywhere the panel can show
one (`load_schema`, `connect`, `schema_from_*`, `GrpcOutcome::error`). `GrpcCall.error`
in the history list stays a `String` via `one_line()` — one row, one line.

- **`error_chain(&dyn Error)` is the single biggest win.** tonic transport failures
  render as the bare words `transport error`; connection-refused, DNS and certificate
  facts all live in the `source()` chain underneath.
- A non-OK **status is not an error** — it's the server's answer. `status_card` renders it
  distinctly from `error_card` so nothing about the local configuration looks suspect,
  and `status_hint` adds what the code usually means *for the operator of this panel*
  (e.g. `Unimplemented` → check the method name and that this is the right server).
- A plaintext client against a TLS port fails as a corrupt HTTP/2 frame; `connect`
  detects that and names `grpcs://` as the fix.

## Getting values out of a response

GPUI text is **not mouse-selectable**, so a response can be read but not taken unless the
panel offers copy explicitly. Two levels:

- `⧉ Copy` in the response header takes the whole of whichever pane is showing (body,
  status, error block, or trailers) — `response_text` decides which.
- Every body line and every trailer row is click-to-copy. `grpc::json_line_value` strips
  the key, the quotes, the trailing comma and the JSON escapes, so what lands on the
  clipboard pastes straight into a request cell. Lines that open an object or array have
  no scalar value and copy as they read.

Bounded at `COPY_LINES` (400) rows: past that the body renders as one block, because the
per-line element count costs more than the affordance is worth and `Copy` still takes
everything in one click.

`cx.write_to_clipboard(ClipboardItem::new_string(text))` is the app-wide pattern
(`terminal.rs:1242`, `code_editor.rs:317`).

**Not built: response → request chaining.** Click-to-copy is the manual 80%. The real
version needs a value path expression (`$.user.id`), somewhere to bind it (the environment
manifest is the obvious home, since `{{vars}}` already interpolate into every cell), and a
decision about when extraction re-runs. Worth doing only alongside a request-sequence
concept — a single saved call has nothing to chain to.

## Reuse confirmed from the existing HTTP tool

- `http::host_allowed` works unchanged on a bare `host:port`: `host_of("localhost:50051")`
  returns `"localhost"` (no `://` → whole string → split on `:`). The SSRF gate is
  inherited for free.
- `views/mcp_host.rs:32` `McpHostHandle::build` is the pattern for the tokio runtime
  (leaked single-worker `enable_all`, `Handle` in `ShellDeps`).
- **Do not** copy its `block_on`: a gRPC call can hang for seconds and would freeze the
  UI thread. Use `handle.spawn(fut)` and await the `JoinHandle` inside `cx.spawn` —
  a `JoinHandle` is pollable from GPUI's executor.
