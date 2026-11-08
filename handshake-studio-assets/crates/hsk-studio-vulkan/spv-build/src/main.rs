//! spv-build: WGSL to SPIR-V for hsk-studio-vulkan's UI pass, offline (decision O3, SMB-VK-UI-PASS-1). naga 27.0.3
//! as a library: parse `shaders/ui.wgsl`, validate with `ValidationFlags::all()` and the capability set the shader
//! needs, then `write_vec` once per entry point with every `Options` field set explicitly. Never `Options::default()`:
//! it adds `WriterFlags::DEBUG` when this tool is built with debug assertions, so debug and release builds would emit
//! different bytes. Writes `shaders/ui.<entry>.spv` and `shaders/manifest.json`; `--check` recompiles and compares
//! the bytes, writing nothing (exit 1 on a difference).
//!
//! Run from a shell that sourced builders/studio-env.sh, from the workspace root:
//!   cargo run --offline --locked --manifest-path crates/hsk-studio-vulkan/spv-build/Cargo.toml \
//!     --target-dir "$CARGO_TARGET_DIR/spv-build" -- [--check]
use naga::back::spv::{self, BindingInfo, BindingMap, Capability, Options, PipelineOptions, WriterFlags, ZeroInitializeWorkgroupMemoryMode};
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SOURCE: &str = "shaders/ui.wgsl";
const REGENERATE: &str =
    "cargo run --offline --locked --manifest-path crates/hsk-studio-vulkan/spv-build/Cargo.toml --target-dir \"$CARGO_TARGET_DIR/spv-build\"";
const ENTRIES: [(&str, naga::ShaderStage, &str); 2] =
    [("vs_main", naga::ShaderStage::Vertex, "vertex"), ("fs_main", naga::ShaderStage::Fragment, "fragment")];

/// Every writer option, explicit. lang_version 1.6 (what wgpu-hal picks on Vulkan 1.3 devices);
/// ADJUST_COORDINATE_SPACE only (WGSL keeps WebGPU's clip space; LABEL_VARYINGS off, some drivers read it as
/// semantic); no fake bindings, so every resource is in the explicit binding map (group 0 bindings 0..=2 to set 0
/// bindings 0..=2: glyph page, icon page, image); capabilities limited to Shader and ImageQuery (the Restrict image
/// bounds check clamps texel coordinates with OpImageQuerySize; both are core Vulkan capabilities); every bounds check
/// Restrict; no debug info.
fn options() -> Options<'static> {
    let mut capabilities = naga::FastHashSet::default();
    capabilities.insert(Capability::Shader);
    capabilities.insert(Capability::ImageQuery);
    let mut binding_map = BindingMap::default();
    for binding in 0..3 {
        binding_map.insert(naga::ResourceBinding { group: 0, binding }, BindingInfo { descriptor_set: 0, binding, binding_array_size: None });
    }
    Options {
        lang_version: (1, 6),
        flags: WriterFlags::ADJUST_COORDINATE_SPACE,
        fake_missing_bindings: false,
        binding_map,
        capabilities: Some(capabilities),
        bounds_check_policies: BoundsCheckPolicies {
            index: BoundsCheckPolicy::Restrict,
            buffer: BoundsCheckPolicy::Restrict,
            image_load: BoundsCheckPolicy::Restrict,
            binding_array: BoundsCheckPolicy::Restrict,
        },
        zero_initialize_workgroup_memory: ZeroInitializeWorkgroupMemoryMode::None,
        force_loop_bounding: false,
        use_storage_input_output_16: false,
        debug_info: None,
    }
}

/// The option values as the manifest records them (and hashes).
const OPTIONS_JSON: &str = concat!(
    "{\"lang_version\":\"1.6\",\"flags\":\"ADJUST_COORDINATE_SPACE\",\"fake_missing_bindings\":false,",
    "\"binding_map\":[\"0.0->0.0\",\"0.1->0.1\",\"0.2->0.2\"],\"capabilities\":[\"Shader\",\"ImageQuery\"],",
    "\"bounds_check_policies\":{\"index\":\"Restrict\",\"buffer\":\"Restrict\",\"image_load\":\"Restrict\",\"binding_array\":\"Restrict\"},",
    "\"zero_initialize_workgroup_memory\":\"None\",\"force_loop_bounding\":false,\"use_storage_input_output_16\":false,",
    "\"debug_info\":\"none\",\"validation_flags\":\"all\",\"validation_capabilities\":[\"PUSH_CONSTANT\"]}"
);

fn main() -> ExitCode {
    let check = std::env::args().skip(1).any(|a| a == "--check");
    match run(check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spv-build: {e}");
            ExitCode::FAILURE
        }
    }
}

fn crate_dir() -> PathBuf {
    let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    tool.parent().map(Path::to_path_buf).unwrap_or(tool)
}

fn run(check: bool) -> Result<(), String> {
    let dir = crate_dir();
    let src_path = dir.join(SOURCE);
    let source = std::fs::read_to_string(&src_path).map_err(|e| format!("{}: {e}", src_path.display()))?;
    let module = naga::front::wgsl::parse_str(&source).map_err(|e| e.emit_to_string(&source))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::PUSH_CONSTANT)
        .validate(&module)
        .map_err(|e| format!("validation: {e:?}"))?;
    let opts = options();
    let mut entries = Vec::new();
    let mut differ = Vec::new();
    for (entry, stage, stage_name) in ENTRIES {
        let pipeline = PipelineOptions { shader_stage: stage, entry_point: entry.to_owned() };
        let words = spv::write_vec(&module, &info, &opts, Some(&pipeline)).map_err(|e| format!("{entry}: {e:?}"))?;
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let rel = format!("shaders/ui.{entry}.spv");
        let path = dir.join(&rel);
        if check {
            if std::fs::read(&path).ok().as_deref() != Some(&bytes[..]) {
                differ.push(rel.clone());
            }
        } else {
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        entries.push(format!(
            "{{\"entry\":\"{entry}\",\"stage\":\"{stage_name}\",\"spv\":\"{rel}\",\"sha256\":\"{}\",\"words\":{}}}",
            sha256_hex(&bytes),
            words.len()
        ));
    }
    let manifest = format!(
        "{{\"schema\":\"hsk.studio.vulkan.spv_manifest@1\",\"source\":\"{SOURCE}\",\"source_sha256\":\"{}\",\"naga\":\"27.0.3\",\"tool\":\"spv-build {}\",\"options\":{OPTIONS_JSON},\"options_sha256\":\"{}\",\"entries\":[{}],\"regenerate\":\"{}\"}}\n",
        sha256_hex(source.as_bytes()),
        env!("CARGO_PKG_VERSION"),
        sha256_hex(OPTIONS_JSON.as_bytes()),
        entries.join(","),
        REGENERATE.replace('"', "\\\""),
    );
    let manifest_path = dir.join("shaders/manifest.json");
    if check {
        if std::fs::read_to_string(&manifest_path).ok().as_deref() != Some(manifest.as_str()) {
            differ.push("shaders/manifest.json".to_owned());
        }
        if !differ.is_empty() {
            return Err(format!("out of date: {}; regenerate with: {REGENERATE}", differ.join(", ")));
        }
        println!("spv-build --check: {} modules and the manifest match", ENTRIES.len());
    } else {
        std::fs::write(&manifest_path, manifest).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        println!("spv-build: wrote {} modules and shaders/manifest.json", ENTRIES.len());
    }
    Ok(())
}

/// SHA-256 (FIPS 180-4), lowercase hex; dependency-free so the tool's closure stays naga's.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
