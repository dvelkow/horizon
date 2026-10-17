//! Isolated validation/transpilation process for the approval portal.
//! The gateway owns wall-clock/process limits and publishes only a successful result.
use horizon_mods::{Content, Package, Status, manifest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use wasm_encoder::reencode::{self, Reencode};

const MAX_UPLOAD: u64 = 64 << 20;
const MAX_EXPANDED: u64 = 128 << 20;

fn main() -> ExitCode {
    match run() {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({"ok":false,"error":error}));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Value, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing {name}"))
    };
    validate(&flag("--input")?, &flag("--browser-out")?)
}

/// Checks the package at `input`; a client component's browser bundle goes
/// in `out`.
fn validate(input: &Path, out: &Path) -> Result<Value, String> {
    let input = input.to_owned();
    preflight(&input)?;
    let package = Package::read(&input)?;
    // The same fences and sizes the game applies. Blocks and items cannot
    // join a running world: approving a content mod restarts the hosted
    // world, and every browser loads the same set before it joins.
    let content = Content::read(&input, &package.manifest)?;
    let content = json!({"blocks":content.blocks.len(),"items":content.items.len(),"files":content.files.len()});
    let checks = json!({"manifest":true,"imports":true,"content":true,"archiveCaps":true});
    let Some(wasm) = package.wasm.as_ref() else {
        // A content-only mod: data, no code to sandbox or transpile.
        return Ok(
            json!({"ok":true,"manifest":package.manifest,"code":false,"content":content,"browser":null,"checks":checks}),
        );
    };
    let imports = manifest::component_imports(wasm)?;
    package
        .manifest
        .check_imports(imports.iter().map(String::as_str))?;
    let runtime = if package.manifest.side == horizon_mods::Side::Server {
        horizon_mods::Mods::load_server(std::slice::from_ref(&input), |_| true, |_| false)
    } else {
        horizon_mods::Mods::load(std::slice::from_ref(&input), |_| true, |_| false)
    };
    let entry = runtime
        .entries()
        .first()
        .ok_or("sandbox did not produce a result")?;
    if let Status::Failed(reason) = &entry.status {
        return Err(format!("sandbox init: {reason}"));
    }
    if !runtime.is_running() {
        return Err("sandbox did not start the component".into());
    }
    let mut checks = checks;
    checks["sandboxInitFuel"] = true.into();
    if package.manifest.side == horizon_mods::Side::Server {
        return Ok(
            json!({"ok":true,"manifest":package.manifest,"code":true,"content":content,"browser":null,"checks":checks}),
        );
    }
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    if fs::read_dir(out)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("browser output directory must be empty".into());
    }
    let component = out.join("component.wasm");
    fs::write(&component, wasm).map_err(|e| e.to_string())?;
    let jco = std::env::var_os("HORIZON_JCO").unwrap_or_else(|| "jco".into());
    let result = Command::new(jco)
        .arg("transpile")
        .arg(&component)
        .args([
            "--name",
            "mod",
            "--instantiation",
            "async",
            "--no-nodejs-compat",
            "--no-wasi-shim",
            "--base64-cutoff",
            "0",
            "--no-typescript",
            "--quiet",
            "-o",
        ])
        .arg(out)
        .output()
        .map_err(|e| format!("jco: {e}"))?;
    fs::remove_file(&component).map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "jco: {}",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(4096)
                .collect::<String>()
        ));
    }
    remove_types(out)?;
    let mut paths: Vec<_> = fs::read_dir(out)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    paths.sort();
    let mut caps = BrowserCaps::default();
    let mut files = Vec::new();
    for path in paths {
        if !path.is_file() {
            return Err("unexpected nested transpiler output".into());
        }
        let mut bytes = fs::read(&path).map_err(|e| e.to_string())?;
        if path.extension().is_some_and(|e| e == "wasm") {
            let mut module = wasm_encoder::Module::new();
            caps.parse_core_module(&mut module, wasmparser::Parser::new(0), &bytes)
                .map_err(|e| format!("browser limits: {e}"))?;
            bytes = module.finish();
            wasmparser::Validator::new()
                .validate_all(&bytes)
                .map_err(|e| e.to_string())?;
            fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        } else if path.extension().is_none_or(|e| e != "js") {
            return Err("unexpected transpiler output type".into());
        }
        if bytes.len() > 64 << 20 {
            return Err("transpiled file exceeds size cap".into());
        }
        files.push(json!({"path":path.file_name().unwrap().to_string_lossy(), "sha256":format!("{:x}",Sha256::digest(&bytes)), "size":bytes.len()}));
    }
    if !out.join("mod.js").is_file() {
        return Err("transpiler did not produce mod.js".into());
    }
    checks["browserMemoryCap"] = (64 << 20).into();
    checks["browserTableCap"] = 10000.into();
    Ok(
        json!({"ok":true,"manifest":package.manifest,"code":true,"content":content,"browser":{"entry":"mod.js","files":files},"checks":checks}),
    )
}

fn preflight(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_UPLOAD {
        return Err("upload must be a regular file of at most 64 MiB".into());
    }
    let mut zip = zip::ZipArchive::new(fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if zip.len() > 4096 {
        return Err("archive contains too many files".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let file = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name();
        if file.is_dir() {
            continue;
        }
        if !names.insert(name.to_owned()) {
            return Err("archive contains duplicate paths".into());
        }
        if file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("archive symlinks are forbidden".into());
        }
        let cap = match name {
            manifest::MANIFEST => 64 << 10,
            manifest::WASM => 32 << 20,
            _ => {
                horizon_mods::content::check_path(name)?;
                16 << 20
            }
        };
        total = total
            .checked_add(file.size())
            .ok_or("archive size overflow")?;
        if file.size() > cap || total > MAX_EXPANDED {
            return Err("archive exceeds expanded size limits".into());
        }
    }
    Ok(())
}

#[derive(Default)]
struct BrowserCaps {
    memories: usize,
    tables: usize,
}
impl Reencode for BrowserCaps {
    type Error = String;
    fn memory_type(
        &mut self,
        ty: wasmparser::MemoryType,
    ) -> Result<wasm_encoder::MemoryType, reencode::Error<String>> {
        self.memories += 1;
        if self.memories > 1
            || ty.memory64
            || ty.shared
            || ty.page_size_log2.is_some()
            || ty.initial > 1024
        {
            return Err(reencode::Error::UserError(
                "live browser mods support one 32-bit memory capped at 64 MiB".into(),
            ));
        }
        let mut result = reencode::utils::memory_type(self, ty);
        result.maximum = Some(result.maximum.unwrap_or(1024).min(1024));
        Ok(result)
    }
    fn table_type(
        &mut self,
        ty: wasmparser::TableType,
    ) -> Result<wasm_encoder::TableType, reencode::Error<String>> {
        self.tables += 1;
        if self.tables > 8 || ty.table64 || ty.initial > 10000 {
            return Err(reencode::Error::UserError(
                "browser table limit exceeded".into(),
            ));
        }
        let mut result = reencode::utils::table_type(self, ty)?;
        result.maximum = Some(result.maximum.unwrap_or(10000).min(10000));
        Ok(result)
    }
}

fn remove_types(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            remove_types(&path)?;
            fs::remove_dir(&path).map_err(|e| e.to_string())?;
        } else if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".d.ts"))
        {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clamps_unbounded_memory_and_rejects_oversized_memory() {
        let ty = wasmparser::MemoryType {
            memory64: false,
            shared: false,
            initial: 17,
            maximum: None,
            page_size_log2: None,
        };
        assert_eq!(
            BrowserCaps::default().memory_type(ty).unwrap().maximum,
            Some(1024)
        );
        assert!(
            BrowserCaps::default()
                .memory_type(wasmparser::MemoryType {
                    initial: 1025,
                    ..ty
                })
                .is_err()
        );
        let mut cap = BrowserCaps::default();
        cap.memory_type(ty).unwrap();
        assert!(cap.memory_type(ty).is_err());
    }

    const RUBY: &str = r#"
id = "ruby"
name = "Ruby"
version = "0.1.0"
api = "0.2"
side = "both"
capabilities = ["registries"]
"#;
    const BLOCK: &[u8] = br#"{"template": "minecraft:iron_block"}"#;

    /// Validates a content-only package with `files`; the scratch folder goes.
    fn content_mod(name: &str, manifest: &str, files: &[(&str, &[u8])]) -> Result<Value, String> {
        let dir =
            std::env::temp_dir().join(format!("horizon-validator-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("package.hmod");
        fs::write(&input, Package::write_files(manifest, None, files).unwrap()).unwrap();
        let result = validate(&input, &dir.join("browser"));
        fs::remove_dir_all(&dir).unwrap();
        result
    }

    #[test]
    fn content_mods_pass_without_code_and_keep_their_fences() {
        let files: &[(&str, &[u8])] = &[
            ("data/ruby/horizon/block/ruby_block.json", BLOCK),
            ("data/minecraft/tags/block/mineable/pickaxe.json", b"{}"),
            ("assets/ruby/lang/en_us.json", b"{}"),
        ];
        let ok = content_mod("ok", RUBY, files).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["code"], false);
        assert_eq!(ok["browser"], Value::Null);
        assert_eq!(ok["content"], json!({"blocks": 1, "items": 0, "files": 3}));
        assert_eq!(ok["manifest"]["capabilities"], json!(["registries"]));
        // The fences still apply: vanilla's files and other namespaces.
        let vanilla = content_mod(
            "vanilla",
            RUBY,
            &[("assets/minecraft/textures/block/stone.png", b"x")],
        );
        assert!(vanilla.unwrap_err().contains("own namespace"));
        // A mod owning vanilla's namespace would replace vanilla's files.
        let minecraft = RUBY.replace("id = \"ruby\"", "id = \"minecraft\"");
        let e = content_mod(
            "minecraft",
            &minecraft,
            &[("data/minecraft/horizon/block/a.json", BLOCK)],
        )
        .unwrap_err();
        assert!(e.contains("namespace is shared"), "{e}");
    }
}
