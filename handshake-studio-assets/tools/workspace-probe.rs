//! STUDIO-WORKSPACE bootstrap consumer. Std-only; compile directly with pinned rustc.
//! No compilation/native/embedding claim is made by this input-selection inspection.
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
const LIMIT: usize = 1_048_576;
const MAX_FILES: usize = 256;
const TOOLCHAIN: &str = "1.97.1";
#[derive(Clone, Debug)]
enum Value {
    Text(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Atom(String),
}
impl Value {
    fn text(&self) -> Result<&str> {
        match self {
            Self::Text(s) => Ok(s),
            _ => Err("expected_string".into()),
        }
    }
    fn list(&self) -> Result<Vec<String>> {
        match self {
            Self::List(v) => v.iter().map(|x| x.text().map(str::to_owned)).collect(),
            _ => Err("expected_string_array".into()),
        }
    }
    fn boolean(&self) -> Result<bool> {
        match self {
            Self::Atom(s) if s == "true" => Ok(true),
            Self::Atom(s) if s == "false" => Ok(false),
            _ => Err("expected_boolean".into()),
        }
    }
}
type Table = BTreeMap<String, Value>;
struct Document {
    tables: Vec<(String, Table)>,
}
impl Document {
    fn table(&self, name: &str) -> Result<&Table> {
        self.tables
            .iter()
            .find(|(s, _)| s == name)
            .map(|(_, v)| v)
            .ok_or_else(|| format!("missing_table:{name}"))
    }
    fn get(&self, table: &str, key: &str) -> Result<&Value> {
        self.table(table)?
            .get(key)
            .ok_or_else(|| format!("missing_key:{table}:{key}"))
    }
    fn text(&self, t: &str, k: &str) -> Result<&str> {
        self.get(t, k)?.text()
    }
}
// Deliberately bounded TOML subset: basic/literal single-line strings, dotted table
// headers, arrays, inline tables, bool/integer atoms. Unsupported syntax is rejected.
struct Parser {
    data: Vec<char>,
    at: usize,
}
impl Parser {
    fn new(s: &str) -> Self {
        Self {
            data: s.chars().collect(),
            at: 0,
        }
    }
    fn peek(&self) -> Option<char> {
        self.data.get(self.at).copied()
    }
    fn take(&mut self) -> Result<char> {
        let c = self.peek().ok_or("unexpected_eof")?;
        self.at += 1;
        Ok(c)
    }
    fn space(&mut self, newlines: bool) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => self.at += 1,
                Some('\n') if newlines => self.at += 1,
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1
                    }
                }
                _ => break,
            }
        }
    }
    fn expect(&mut self, c: char) -> Result<()> {
        if self.take()? == c {
            Ok(())
        } else {
            Err(format!("syntax_expected:{c}"))
        }
    }
    fn string(&mut self) -> Result<String> {
        let q = self.take()?;
        let mut s = String::new();
        loop {
            let c = self.take()?;
            if c == q {
                return Ok(s);
            }
            if c == '\n' || c == '\r' || c.is_control() {
                return Err("invalid_string".into());
            }
            if c == '\\' && q == '"' {
                let e = self.take()?;
                s.push(match e {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err("unsupported_escape".into()),
                })
            } else {
                s.push(c)
            }
        }
    }
    fn key(&mut self) -> Result<String> {
        self.space(false);
        if matches!(self.peek(), Some('"' | '\'')) {
            return self.string();
        }
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.at += 1
        }
        if self.at == start {
            return Err("invalid_key".into());
        }
        Ok(self.data[start..self.at].iter().collect())
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > 16 {
            return Err("depth_limit".into());
        }
        self.space(true);
        match self.peek() {
            Some('"' | '\'') => Ok(Value::Text(self.string()?)),
            Some('[') => {
                self.at += 1;
                let mut v = vec![];
                loop {
                    self.space(true);
                    if self.peek() == Some(']') {
                        self.at += 1;
                        break;
                    }
                    if v.len() >= 256 {
                        return Err("array_limit".into());
                    }
                    v.push(self.value(depth + 1)?);
                    self.space(true);
                    if self.peek() == Some(']') {
                        self.at += 1;
                        break;
                    }
                    self.expect(',')?
                }
                Ok(Value::List(v))
            }
            Some('{') => {
                self.at += 1;
                let mut m = Table::new();
                loop {
                    self.space(false);
                    if self.peek() == Some('}') {
                        self.at += 1;
                        break;
                    }
                    let k = self.key()?;
                    self.space(false);
                    self.expect('=')?;
                    let v = self.value(depth + 1)?;
                    if m.insert(k, v).is_some() {
                        return Err("duplicate_inline_key".into());
                    }
                    self.space(false);
                    if self.peek() == Some('}') {
                        self.at += 1;
                        break;
                    }
                    self.expect(',')?
                }
                Ok(Value::Map(m))
            }
            Some(_) => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|c| !c.is_whitespace() && !",]}#".contains(c))
                {
                    self.at += 1
                }
                let s: String = self.data[start..self.at].iter().collect();
                if s == "true"
                    || s == "false"
                    || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                {
                    Ok(Value::Atom(s))
                } else {
                    Err("unsupported_toml_value".into())
                }
            }
            None => Err("missing_value".into()),
        }
    }
    fn parse(mut self) -> Result<Document> {
        let mut tables = vec![(String::new(), Table::new())];
        let mut current = 0;
        let mut seen = BTreeSet::new();
        loop {
            self.space(true);
            if self.peek().is_none() {
                break;
            }
            if self.peek() == Some('[') {
                self.at += 1;
                let array = self.peek() == Some('[');
                if array {
                    self.at += 1
                }
                let mut parts = vec![self.key()?];
                loop {
                    self.space(false);
                    if self.peek() != Some('.') {
                        break;
                    }
                    self.at += 1;
                    parts.push(self.key()?)
                }
                self.space(false);
                self.expect(']')?;
                if array {
                    self.expect(']')?
                }
                let mut name = parts.join(".");
                if array {
                    if name != "package" && name != "example" {
                        return Err("unsupported_array_table".into());
                    }
                    name.push_str("[]");
                }
                if !array && !seen.insert(name.clone()) {
                    return Err(format!("duplicate_table:{name}"));
                }
                if tables.len() >= 512 {
                    return Err("table_limit".into());
                }
                tables.push((name, Table::new()));
                current = tables.len() - 1;
            } else {
                let k = self.key()?;
                self.space(false);
                self.expect('=')?;
                let v = self.value(0)?;
                if tables[current].1.insert(k, v).is_some() {
                    return Err("duplicate_key".into());
                }
            }
            self.space(false);
            if self.peek().is_some() && self.take()? != '\n' {
                return Err("trailing_syntax".into());
            }
        }
        Ok(Document { tables })
    }
}
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn strings(v: impl IntoIterator<Item = String>) -> String {
    format!(
        "[{}]",
        v.into_iter()
            .map(|s| quoted(&s))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(|_| "unreadable_input")?;
    let mut bytes = vec![];
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "read_error")?;
    if bytes.len() > LIMIT {
        return Err("input_size_limit".into());
    }
    Ok(bytes)
}
fn doc(bytes: &[u8]) -> Result<Document> {
    Parser::new(std::str::from_utf8(bytes).map_err(|_| "invalid_utf8")?).parse()
}
fn inside(root: &Path, relative: &str) -> Result<PathBuf> {
    let p = Path::new(relative);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("unsafe_relative_path".into());
    }
    let p = root
        .join(p)
        .canonicalize()
        .map_err(|_| "absent_selected_path")?;
    if !p.starts_with(root) {
        return Err("path_outside_workspace".into());
    }
    Ok(p)
}
fn sha256(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut bytes = input.to_vec();
    bytes.push(128);
    while bytes.len() % 64 != 56 {
        bytes.push(0)
    }
    bytes.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
    for chunk in bytes.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, c) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(c.try_into().unwrap())
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            w[i] = w[i - 16]
                .wrapping_add(a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3))
                .wrapping_add(w[i - 7])
                .wrapping_add(b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10))
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z] = h;
        for i in 0..64 {
            let t = z
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let u = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            z = g;
            g = f;
            f = e;
            e = d.wrapping_add(t);
            d = c;
            c = b;
            b = a;
            a = t.wrapping_add(u)
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
            *x = x.wrapping_add(y)
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
struct Inputs {
    bytes: BTreeMap<String, Vec<u8>>,
    hashes: BTreeMap<String, String>,
}
impl Inputs {
    fn load(&mut self, root: &Path, path: &str) -> Result<Document> {
        if self.bytes.len() >= MAX_FILES {
            return Err("file_count_limit".into());
        }
        let bytes = read(&inside(root, path)?)?;
        let parsed = doc(&bytes)?;
        self.hashes.insert(path.into(), sha256(&bytes));
        self.bytes.insert(path.into(), bytes);
        Ok(parsed)
    }
    fn stable(&self, root: &Path) -> Result<()> {
        for (path, bytes) in &self.bytes {
            if read(&inside(root, path)?)? != *bytes {
                return Err("input_changed_during_inspection".into());
            }
        }
        Ok(())
    }
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn list(doc: &Document, t: &str, k: &str) -> Result<Vec<String>> {
    doc.get(t, k)?.list()
}
fn unique(v: &[String], error: &str) -> Result<()> {
    let mut set = BTreeSet::new();
    for x in v {
        if !set.insert(x) {
            return Err(error.into());
        }
    }
    Ok(())
}
fn inherited_text<'a>(d: &'a Document, w: &'a Document, key: &str) -> Result<&'a str> {
    match d.get("package", key)? {
        Value::Map(m) if m.get("workspace").is_some_and(|v| v.boolean() == Ok(true)) => {
            w.text("workspace.package", key)
        }
        v => v.text(),
    }
}
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const MAX_REGISTRY_RECORDS: usize = 256;
fn stable_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
// Owner -> approved direct crates.io libraries (mirrors workspace.metadata.studio.registry.OWNER.direct).
// Maintained by the workspace steward's registry sync tool; only edit between the markers.
// BEGIN GENERATED APPROVED
const APPROVED: &[(&str, &[&str])] = &[
    ("hsk-studio-folio", &["schemars", "serde", "serde_json"]),
    ("hsk-studio-chronicle", &["schemars", "serde", "serde_json"]),
    ("hsk-studio-prism", &["moxcms", "sha2"]),
    ("hsk-studio-pigment", &["serde", "serde_json"]),
    ("hsk-studio-nib", &["kurbo", "sha2"]),
    ("hsk-studio-type", &["bitflags", "bytemuck", "serde_json", "sha2", "unicode-bidi-mirroring", "unicode-ccc", "unicode-properties", "unicode-script"]),
];
// END GENERATED APPROVED
// Isolated native/FFI provider owners (STUDIO_MODULES): only these may hold `-sys`/native-ffi closure records.
const NATIVE_OWNERS: &[&str] = &[
    "hsk-studio-render-gpu",
    "hsk-studio-score-device",
    "hsk-studio-score-plugin",
    "hsk-studio-reel-native",
    "hsk-studio-motion-js",
];
// Restricted libraries allowed only in their designated owner's registry closure.
const DESIGNATED: &[(&str, &[&str])] = &[
    ("wgpu", &["hsk-studio-render-gpu"]),
    ("egui", &["hsk-studio-controls"]),
    ("ffmpeg-sys-next", &["hsk-studio-reel-native"]),
];
const ALWAYS_FORBIDDEN: &[&str] = &[
    "handshake_core",
    "handshake_native",
    "surrealdb",
    "rocksdb",
    "eframe",
    "image",
    "libsqlite3-sys",
];
fn native_name(name: &str) -> bool {
    name.ends_with("-sys") || name.ends_with("_sys")
}
fn registry_name_allowed(owner: &str, name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !ALWAYS_FORBIDDEN.contains(&name)
        && DESIGNATED
            .iter()
            .find(|(n, _)| *n == name)
            .is_none_or(|(_, owners)| owners.contains(&owner))
        && (!native_name(name) || NATIVE_OWNERS.contains(&owner))
}
fn as_map(value: &Value) -> Result<&Table> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err("registry_policy_expected_inline_map".into())
    }
}
fn text_key<'a>(map: &'a Table, key: &str) -> Result<&'a str> {
    map.get(key).ok_or("registry_policy_missing_key")?.text()
}
fn exact_keys(map: &Table, allowed: &[&str]) -> Result<()> {
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        Err("registry_policy_unknown_field".into())
    } else {
        Ok(())
    }
}
fn registry_closure(
    owner: &str,
    alias: &str,
    dep: &str,
    specification: &Value,
    workspace: &Document,
    lock: &BTreeMap<(String, String), Table>,
    observed: &mut BTreeSet<String>,
) -> Result<()> {
    // Narrow permission: only owner/library pairs in APPROVED are authorized direct libraries.
    let approved: &[&str] = APPROVED
        .iter()
        .find(|(o, _)| *o == owner)
        .map(|(_, libs)| *libs)
        .ok_or("registry_owner_or_direct_library_not_approved")?;
    if !approved.contains(&dep) {
        return Err("registry_owner_or_direct_library_not_approved".into());
    }
    let actual = as_map(specification)?;
    exact_keys(
        actual,
        &[
            "version",
            "features",
            "default-features",
            "optional",
            "package",
        ],
    )?;
    if actual
        .get("default-features")
        .ok_or("registry_default_policy_missing")?
        .boolean()?
    {
        return Err("registry_implicit_default_features_forbidden".into());
    }
    let policy = workspace.table(&format!("workspace.metadata.studio.registry.{owner}"))?;
    exact_keys(policy, &["direct", "locked"])?;
    let direct = as_map(
        policy
            .get("direct")
            .ok_or("registry_direct_policy_missing")?,
    )?;
    if direct.len() > approved.len() || direct.keys().any(|n| !approved.contains(&n.as_str())) {
        return Err("registry_unapproved_direct_policy".into());
    }
    let pin = as_map(direct.get(dep).ok_or("registry_direct_pin_missing")?)?;
    exact_keys(pin, &["version", "features", "default-features"])?;
    let version = text_key(pin, "version")?;
    if !stable_version(version) || text_key(actual, "version")? != format!("={version}") {
        return Err("registry_exact_version_pin_required".into());
    }
    if pin
        .get("default-features")
        .ok_or("registry_pin_default_policy_missing")?
        .boolean()?
    {
        return Err("registry_pin_defaults_forbidden".into());
    }
    let mut actual_features = actual
        .get("features")
        .ok_or("registry_features_missing")?
        .list()?;
    let mut expected_features = pin
        .get("features")
        .ok_or("registry_pin_features_missing")?
        .list()?;
    unique(&actual_features, "registry_duplicate_features")?;
    unique(&expected_features, "registry_duplicate_pin_features")?;
    actual_features.sort();
    expected_features.sort();
    if actual_features != expected_features {
        return Err("registry_feature_policy_mismatch".into());
    }
    // Aliases must explicitly name the approved library; no private registry or renamed provider.
    if alias != dep && actual.get("package").is_none() {
        return Err("registry_alias_package_missing".into());
    }
    let pins = as_map(policy.get("locked").ok_or("registry_lock_policy_missing")?)?;
    if pins.len() > MAX_REGISTRY_RECORDS {
        return Err("registry_record_limit".into());
    }
    let root = (dep.to_owned(), version.to_owned());
    let mut graph = RegistryGraph::new();
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    while let Some(identity) = pending.pop() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        if visited.len() > MAX_REGISTRY_RECORDS {
            return Err("registry_record_limit".into());
        }
        let (name, version) = &identity;
        if !registry_name_allowed(owner, name) || !stable_version(version) {
            return Err("registry_unapproved_native_or_identity".into());
        }
        let locked = lock.get(&identity).ok_or("registry_package_not_locked")?;
        let key = format!("{name}@{version}");
        let pinned = as_map(pins.get(&key).ok_or("registry_transitive_pin_missing")?)?;
        exact_keys(pinned, &["source", "checksum", "kind"])?;
        // Honest labels: `-sys` records are native-ffi; native-ffi only inside native provider owners.
        match text_key(pinned, "kind")? {
            "pure-rust" if !native_name(name) => {}
            "pure-rust" => return Err("registry_native_kind_required".into()),
            "native-ffi" if NATIVE_OWNERS.contains(&owner) => {}
            _ => return Err("registry_nonpure_policy_forbidden".into()),
        }
        if text_key(locked, "source")? != CRATES_IO_SOURCE
            || text_key(pinned, "source")? != CRATES_IO_SOURCE
        {
            return Err("registry_source_not_crates_io".into());
        }
        let checksum = text_key(locked, "checksum")?;
        if !hex(checksum, 64) || text_key(pinned, "checksum")? != checksum {
            return Err("registry_checksum_pin_mismatch".into());
        }
        observed.insert(format!("{owner}:{key}:{checksum}"));
        if observed.len() > MAX_REGISTRY_RECORDS {
            return Err("registry_record_limit".into());
        }
        if let Some(dependencies) = locked.get("dependencies") {
            for edge in dependencies.list()? {
                let fields: Vec<_> = edge.split_whitespace().collect();
                let target = match fields.as_slice() {
                    [name] => {
                        let candidates: Vec<_> = lock
                            .keys()
                            .filter(|(candidate, _)| candidate.as_str() == *name)
                            .cloned()
                            .collect();
                        if candidates.len() != 1 {
                            return Err("registry_lock_dependency_ambiguous".into());
                        }
                        candidates[0].clone()
                    }
                    [name, version] => ((*name).to_owned(), (*version).to_owned()),
                    [name, version, source] if *source == format!("({CRATES_IO_SOURCE})") => {
                        ((*name).to_owned(), (*version).to_owned())
                    }
                    _ => return Err("registry_lock_edge_unsupported_source".into()),
                };
                graph
                    .entry(identity.clone())
                    .or_default()
                    .insert(target.clone());
                pending.push(target);
                if pending.len() > MAX_REGISTRY_RECORDS {
                    return Err("registry_edge_limit".into());
                }
            }
        }
    }
    let mut done = BTreeSet::new();
    for identity in &visited {
        registry_acyclic(identity, &graph, &mut BTreeSet::new(), &mut done)?;
    }
    Ok(())
}
type LockIdentity = (String, String);
type RegistryGraph = BTreeMap<LockIdentity, BTreeSet<LockIdentity>>;
fn registry_acyclic(
    node: &LockIdentity,
    graph: &RegistryGraph,
    visiting: &mut BTreeSet<LockIdentity>,
    done: &mut BTreeSet<LockIdentity>,
) -> Result<()> {
    if done.contains(node) {
        return Ok(());
    }
    if !visiting.insert(node.clone()) {
        return Err("registry_dependency_cycle".into());
    }
    if let Some(edges) = graph.get(node) {
        for next in edges {
            registry_acyclic(next, graph, visiting, done)?;
        }
    }
    visiting.remove(node);
    done.insert(node.clone());
    Ok(())
}
fn inspect(args: &[String]) -> Result<String> {
    let mut opts = BTreeMap::new();
    let mut selected = vec![];
    let mut features = BTreeMap::<String, BTreeSet<String>>::new();
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        let val = args.get(i + 1).ok_or("missing_argument_value")?;
        match key.as_str() {
            "--package" => selected.push(val.clone()),
            "--feature" => {
                let (p, f) = val
                    .split_once('/')
                    .ok_or("feature_requires_package_slash_name")?;
                features.entry(p.into()).or_default().insert(f.into());
            }
            "--root" | "--provenance" | "--revision" | "--tree" | "--provider" => {
                if opts.insert(key.clone(), val.clone()).is_some() {
                    return Err("duplicate_argument".into());
                }
            }
            _ => return Err("unknown_argument".into()),
        }
        i += 2;
    }
    unique(&selected, "duplicate_selection")?;
    let required = |k: &str| {
        opts.get(k)
            .map(String::as_str)
            .ok_or_else(|| format!("missing_argument:{k}"))
    };
    let root = Path::new(required("--root")?)
        .canonicalize()
        .map_err(|_| "unreadable_workspace")?;
    let revision = required("--revision")?;
    let tree = required("--tree")?;
    if !hex(revision, 40) || !hex(tree, 40) {
        return Err("invalid_git_identity".into());
    }
    if opts.get("--provider").is_some_and(|s| s != "none") {
        return Err("unsupported_provider_use_owning_module_selector".into());
    }
    let provenance_bytes = read(Path::new(required("--provenance")?))?;
    let provenance = doc(&provenance_bytes)?;
    if provenance.text("source", "repository")? != "https://github.com/Nuntissura/assets"
        || provenance.text("source", "subtree")? != "handshake-studio-assets"
        || provenance.text("source", "revision")? != revision
        || provenance.text("source", "tree")? != tree
    {
        return Err("provenance_identity_mismatch".into());
    }
    let mut inputs = Inputs {
        bytes: BTreeMap::new(),
        hashes: BTreeMap::new(),
    };
    let manifest = inputs.load(&root, "Cargo.toml")?;
    let toolchain = inputs.load(&root, "rust-toolchain.toml")?;
    let lock = inputs.load(&root, "Cargo.lock")?;
    if toolchain.text("toolchain", "channel")? != TOOLCHAIN
        || manifest.text("workspace.metadata.studio", "toolchain")? != TOOLCHAIN
        || manifest.text("workspace.package", "rust-version")? != TOOLCHAIN
    {
        return Err("unsupported_toolchain".into());
    }
    if manifest.text("workspace", "resolver")? != "3"
        || manifest.text("workspace.metadata.studio", "repository")?
            != "https://github.com/Nuntissura/assets"
        || manifest.text("workspace.metadata.studio", "subtree")? != "handshake-studio-assets"
    {
        return Err("workspace_identity_mismatch".into());
    }
    if !matches!(lock.get("","version")?,Value::Atom(s) if s=="4") {
        return Err("unsupported_lock_version".into());
    }
    let declared = list(&manifest, "workspace.metadata.studio", "declared-members")?;
    let members = list(&manifest, "workspace", "members")?;
    unique(&declared, "duplicate_declared_member")?;
    unique(&members, "duplicate_member")?;
    let mut available = BTreeMap::new();
    for path in &declared {
        if !path.starts_with("crates/hsk-studio-")
            || path.contains(['*', '?'])
            || Path::new(path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("invalid_declared_member".into());
        }
        let name = path.rsplit('/').next().ok_or("invalid_member")?;
        if available.insert(name.to_owned(), path.clone()).is_some() {
            return Err("duplicate_package_name".into());
        }
    }
    for path in &members {
        if !declared.contains(path) {
            return Err("undeclared_workspace_member".into());
        }
    }
    for p in &selected {
        if !available.contains_key(p) {
            return Err("unknown_package".into());
        }
    }
    for p in features.keys() {
        if !selected.contains(p) {
            return Err("feature_for_unselected_package".into());
        }
    }
    let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
    let mut pending = selected.clone();
    let mut visited = BTreeSet::new();
    let mut locked = BTreeMap::new();
    let mut registry_observed = BTreeSet::new();
    for (section, table) in &lock.tables {
        if section == "package[]" {
            let name = table.get("name").ok_or("lock_package_name")?.text()?;
            let version = table.get("version").ok_or("lock_package_version")?.text()?;
            if locked
                .insert((name.to_owned(), version.to_owned()), table.clone())
                .is_some()
            {
                return Err("duplicate_lock_package".into());
            }
        }
    }
    while let Some(package) = pending.pop() {
        if !visited.insert(package.clone()) {
            continue;
        }
        if visited.len() > 35 {
            return Err("closure_limit".into());
        }
        let dir = available.get(&package).ok_or("undeclared_dependency")?;
        if !members.contains(dir) {
            return Err("selected_package_not_registered".into());
        }
        let d = inputs.load(&root, &format!("{dir}/Cargo.toml"))?;
        if d.text("package", "name")? != package {
            return Err("package_name_mismatch".into());
        }
        let version = inherited_text(&d, &manifest, "version")?;
        if !locked.contains_key(&(package.clone(), version.to_owned())) {
            return Err("selected_package_not_locked".into());
        }
        let requested = features.get(&package).cloned().unwrap_or_default();
        let ft = d
            .tables
            .iter()
            .find(|(s, _)| s == "features")
            .map(|(_, t)| t);
        let mut active = requested.clone();
        let mut queue: Vec<String> = requested.into_iter().collect();
        let mut dep_features = BTreeSet::new();
        while let Some(f) = queue.pop() {
            let entries = ft
                .and_then(|t| t.get(&f))
                .ok_or("unknown_feature")?
                .list()?;
            for entry in entries {
                if let Some(dep) = entry.strip_prefix("dep:") {
                    dep_features.insert(dep.to_owned());
                } else if entry.contains('/') {
                    return Err("unsupported_dependency_feature_selector".into());
                } else if active.insert(entry.clone()) {
                    queue.push(entry)
                }
                if active.len() + dep_features.len() > 256 {
                    return Err("feature_limit".into());
                }
            }
        }
        let allowed = list(
            &manifest,
            &format!("workspace.metadata.studio.dependencies.{package}"),
            "allowed",
        )?;
        for (section, table) in &d.tables {
            if section == "dependencies"
                || section == "build-dependencies"
                || section == "dev-dependencies"
                || section.ends_with(".dependencies")
                || section.ends_with(".build-dependencies")
                || section.ends_with(".dev-dependencies")
            {
                for (alias, spec) in table {
                    let mut spec = spec.clone();
                    let inherited = matches!(&spec,Value::Map(m) if m.get("workspace").is_some_and(|v|v.boolean()==Ok(true)));
                    if inherited {
                        if as_map(&spec)?.len() != 1 {
                            return Err("inherited_dependency_override_unsupported".into());
                        }
                        spec = manifest.get("workspace.dependencies", alias)?.clone()
                    }
                    if let Value::Map(fields) = &spec {
                        if fields.keys().any(|k| {
                            ["git", "branch", "tag", "rev", "registry", "registry-index"]
                                .contains(&k.as_str())
                        }) {
                            return Err("dependency_source_override_forbidden".into());
                        }
                    }
                    let (dep, path, optional) = match &spec {
                        Value::Map(m) => (
                            m.get("package")
                                .map(Value::text)
                                .transpose()?
                                .unwrap_or(alias),
                            m.get("path").map(Value::text).transpose()?,
                            m.get("optional")
                                .map(Value::boolean)
                                .transpose()?
                                .unwrap_or(false),
                        ),
                        Value::Text(_) => (alias.as_str(), None, false),
                        _ => return Err("unsupported_dependency_shape".into()),
                    };
                    if matches!(
                        dep,
                        "handshake_core" | "handshake_native" | "surrealdb" | "rocksdb"
                    ) || ((package == "hsk-studio-accord" || package == "hsk-studio-folio")
                        && matches!(
                            dep,
                            "egui"
                                | "eframe"
                                | "wgpu"
                                | "image"
                                | "ffmpeg-sys-next"
                                | "libsqlite3-sys"
                        ))
                    {
                        return Err("forbidden_dependency_edge".into());
                    }
                    if optional && !dep_features.contains(alias) && !active.contains(alias) {
                        continue;
                    }
                    if let Some(path) = path {
                        let actual = (if inherited {
                            root.clone()
                        } else {
                            root.join(dir)
                        })
                        .join(path)
                        .canonicalize()
                        .map_err(|_| "absent_selected_dependency_path")?;
                        if !actual.starts_with(&root) {
                            return Err("dependency_outside_workspace".into());
                        }
                        let expected = available.get(dep).ok_or("undeclared_dependency")?;
                        if actual != inside(&root, expected)? || !allowed.iter().any(|s| s == dep) {
                            return Err("dependency_outside_declared_dag".into());
                        }
                        graph.entry(package.clone()).or_default().insert(dep.into());
                        pending.push(dep.into());
                    } else {
                        registry_closure(
                            &package,
                            alias,
                            dep,
                            &spec,
                            &manifest,
                            &locked,
                            &mut registry_observed,
                        )?;
                    }
                }
            } else if section.starts_with("dependencies.")
                || section.starts_with("build-dependencies.")
                || section.starts_with("dev-dependencies.")
                || section.contains(".dependencies.")
                || section.contains(".build-dependencies.")
                || section.contains(".dev-dependencies.")
            {
                return Err("dependency_subtables_require_inline_contract".into());
            }
        }
    }
    let files = provenance.table("files")?;
    for (path, hash) in &inputs.hashes {
        if files.get(path).ok_or("missing_provenance_file")?.text()? != hash {
            return Err("provenance_file_hash_mismatch".into());
        }
    }
    for (path, hash) in files {
        if !hex(hash.text()?, 64) {
            return Err("invalid_provenance_digest".into());
        }
        if !inputs.hashes.contains_key(path) {
            let bytes = read(&inside(&root, path)?)?;
            if sha256(&bytes) != hash.text()? {
                return Err("provenance_file_hash_mismatch".into());
            }
            inputs.bytes.insert(path.clone(), bytes);
            inputs.hashes.insert(path.clone(), hash.text()?.into());
            if inputs.bytes.len() > MAX_FILES {
                return Err("file_count_limit".into());
            }
        }
    }
    fn acyclic(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<()> {
        if done.contains(node) {
            return Ok(());
        }
        if !visiting.insert(node.into()) {
            return Err("dependency_cycle".into());
        }
        if let Some(edges) = graph.get(node) {
            for next in edges {
                acyclic(next, graph, visiting, done)?;
            }
        }
        visiting.remove(node);
        done.insert(node.into());
        Ok(())
    }
    let mut done = BTreeSet::new();
    for node in &visited {
        acyclic(node, &graph, &mut BTreeSet::new(), &mut done)?;
    }
    inputs.stable(&root)?;
    if read(Path::new(required("--provenance")?))? != provenance_bytes {
        return Err("provenance_changed_during_inspection".into());
    }
    let hashes = inputs
        .hashes
        .iter()
        .map(|(p, h)| format!("{}:{}", quoted(p), quoted(h)))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "{{\"schema\":\"hsk.studio.workspace-inspection@1\",\"status\":\"accepted\",\"proof_kind\":\"input_selection_only\",\"compiled_packages\":false,\"revision\":{},\"tree\":{},\"toolchain\":{},\"selected\":{},\"closure\":{},\"registry_closure\":{},\"declared_count\":{},\"materialized_member_count\":{},\"provenance_sha256\":{},\"inputs_sha256\":{{{}}}}}",
        quoted(revision),
        quoted(tree),
        quoted(TOOLCHAIN),
        strings(selected),
        strings(visited),
        strings(registry_observed),
        declared.len(),
        members.len(),
        quoted(&sha256(&provenance_bytes)),
        hashes
    ))
}
const HELP: &str = r#"{"schema":"hsk.studio.workspace-command@1","owner":"STUDIO-WORKSPACE","command":"workspace-probe","version":1,"operation":"Read-only bounded workspace selection and provenance inspection","usage":"workspace-probe --root SUBTREE --provenance FILE --revision GIT40 --tree GIT40 [--package NAME]* [--feature PACKAGE/NAME]* [--provider none]","bootstrap":"Compile tools/workspace-probe.rs directly with rustc +1.97.1; no Cargo member is needed. Parent selects an external output path. Empty selection validates real manifest, toolchain, lock and provenance inputs; it never proves crate compilation.","inputs":"UTF-8 bounded TOML subset: single-line quoted strings, arrays, inline tables, bool/integer atoms; dotted table headers and array-of-table lock packages/Cargo example targets. Unsupported syntax rejects. Provenance has [source] repository, subtree, revision, tree strings and [files] quoted relative paths to exact SHA-256. Include Cargo.toml, Cargo.lock, rust-toolchain.toml and every selected closure manifest; independently reconcile revision/tree and these hashes to canonical Git before acceptance.","selection":"Explicit registered packages only, no implicit default features. Selected path dependencies must be registered, contained, locked and allowed by the manifest DAG. Unselected planned siblings need not exist. Only owner/library pairs in the APPROVED table (generated from workspace.metadata.studio.registry.OWNER.direct by the steward registry sync tool) are approved exact inline direct registry pins, with explicit no defaults and exact feature sets. Native policy: -sys/_sys records only in native provider owners render-gpu, score-device, score-plugin, reel-native, motion-js and labelled kind=native-ffi; wgpu only in render-gpu, egui only in controls, ffmpeg-sys-next only in reel-native; handshake_core, handshake_native, surrealdb, rocksdb, eframe, image, libsqlite3-sys always reject. Metadata workspace.metadata.studio.registry.OWNER has direct inline maps by library (version,features,default-features=false), and locked inline maps keyed name@version (source,checksum,kind=pure-rust|native-ffi) covering the actual selected transitive lock closure, maximum256records. Source is exactly registry+https://github.com/rust-lang/crates.io-index; lock checksum must be64hex and match independently pinned policy. Git/alternate registries, implicit defaults, unapproved native/host dependencies, ambiguous identities and missing transitive pins reject; no network. Metadata pure classification is an owner-approved selection policy, not independent crate implementation review or compiler feature proof. Host paths and pure-leaf GPU edges still reject.","outputs":"One JSON result on stdout, accepted exit 0; rejection exit 2 with code and recovery. No file writes, network, child processes or foreground UI.","limits":{"file_bytes":1048576,"files":256,"closure_packages":35,"value_depth":16,"array_items":256},"recovery":"Repair only the named malformed/stale selection, file, toolchain, lock or provenance input; repin/reconcile candidate identity independently, then retry affected inspection. Registration/lock updates happen when actual crates materialize. Runtime/embedding/GUI/native proof stays pending.","argus":{"inspect":"same immutable JSON inputs/result","action":"invoke this read-only command with an explicit selection","state":"result includes selected closure and immutable input SHA-256","capture":"caller captures stdout in its granted owner artifact root"},"diagnostics":"Bounded error codes exclude paths and source bytes; accepted results include explicitly inspected relative input paths. Caller owns account/Principal/AccessSpace attribution, grant enforcement and Flight Recorder/internal diagnostics/Palmistry delivery; this local tool does not assert host authorization."}"#;
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = if args == ["--help"] || args == ["--descriptor"] {
        Ok(HELP.to_owned())
    } else {
        inspect(&args)
    };
    let (line, code) = match result {
        Ok(s) => (s, 0),
        Err(e) => (
            format!(
                "{{\"schema\":\"hsk.studio.workspace-inspection@1\",\"status\":\"rejected\",\"code\":{},\"recovery\":\"Consult --descriptor; repair only the rejected input and independently reconcile candidate identity.\"}}",
                quoted(e.split(':').next().unwrap_or("inspection_error"))
            ),
            2,
        ),
    };
    if writeln!(std::io::stdout().lock(), "{line}").is_err() {
        std::process::exit(3)
    }
    std::process::exit(code)
}
