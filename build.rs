use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const BUNDLED_DIR: &str = "bundled";
const COMPRESSION_LEVEL: i32 = 19;

fn main() {
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed={BUNDLED_DIR}");

    embed_bundled_tools();

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("icon.ico");
        resource.set("ProductName", "Vidown");
        resource.set("FileDescription", "Vidown");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=failed to embed icon: {err}");
        }
    }
}

fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(hash, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn embed_bundled_tools() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let mut entries: Vec<PathBuf> = std::fs::read_dir(BUNDLED_DIR)
        .map(|dir| dir.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.is_file()).collect())
        .unwrap_or_default();
    entries.sort();

    let mut bundle_hash = 0xcbf2_9ce4_8422_2325u64;
    let mut files = String::new();
    for path in &entries {
        let name = path.file_name().and_then(|name| name.to_str()).expect("bundled file name is UTF-8");
        let data = std::fs::read(path).unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let file_hash = format!("{:016x}", fnv1a(fnv1a(0xcbf2_9ce4_8422_2325, name.as_bytes()), &data));
        bundle_hash = fnv1a(bundle_hash, file_hash.as_bytes());

        let compressed_path = out_dir.join(format!("{name}.zst"));
        let hash_path = out_dir.join(format!("{name}.hash"));
        if std::fs::read_to_string(&hash_path).ok().as_deref() != Some(file_hash.as_str()) || !compressed_path.is_file() {
            compress(&data, &compressed_path);
            std::fs::write(&hash_path, &file_hash).expect("write hash");
        }

        writeln!(files, "    ({name:?}, include_bytes!({:?})),", compressed_path.display().to_string()).unwrap();
    }

    let source = format!(
        "pub const BUNDLE_ID: &str = \"{bundle_hash:016x}\";\n\
         pub static BUNDLED_FILES: &[(&str, &[u8])] = &[\n{files}];\n"
    );
    std::fs::write(out_dir.join("bundled_tools.rs"), source).expect("write bundled_tools.rs");
}

fn compress(data: &[u8], output_path: &Path) {
    let output = std::fs::File::create(output_path).expect("create compressed file");
    let mut encoder = zstd::Encoder::new(output, COMPRESSION_LEVEL).expect("create zstd encoder");
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get() as u32);
    let _ = encoder.multithread(threads);
    let _ = encoder.long_distance_matching(true);
    std::io::copy(&mut &data[..], &mut encoder).expect("compress bundled file");
    encoder.finish().expect("finish zstd stream");
}
