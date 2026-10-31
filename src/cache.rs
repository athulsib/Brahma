//! First-run resources. When `--resources` is omitted, the generator prepare
//! copies the vendored Java exporters into a per-user cache and builds
//! `facts.json`, the biome-parameter reports and the worldgen bundle there
//! from the player's own Minecraft jars. Jars and the cache stay out of git.
use crate::{Error, GenerateArgs};
use horizon_core::version::VANILLA_VERSION;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Prepared client directory: `--resources`, or the user cache after prepare.
pub fn resolve(args: &GenerateArgs) -> Result<PathBuf, Error> {
    if let Some(path) = &args.resources {
        return Ok(path.clone());
    }
    prepare(args)
}

/// A cache a previous run already prepared, if `facts.json` is there.
pub fn existing() -> Option<PathBuf> {
    let path = resources_dir(&cache_root());
    path.join("facts.json").is_file().then_some(path)
}

fn named_root(name: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(dir).join(name).join(VANILLA_VERSION);
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(dir).join(name).join(VANILLA_VERSION);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("share").join(name).join(VANILLA_VERSION)
}

/// New prepares use `brahma`. A cache already prepared under the old name is
/// reused so the jars are not extracted again.
fn cache_root() -> PathBuf {
    let fresh = named_root("brahma");
    if resources_dir(&fresh).join("facts.json").is_file() {
        return fresh;
    }
    let legacy = named_root("horizon-worldgen");
    if resources_dir(&legacy).join("facts.json").is_file() {
        return legacy;
    }
    fresh
}

fn resources_dir(root: &Path) -> PathBuf {
    root.join(".local")
        .join("horizon-client")
        .join(VANILLA_VERSION)
}

fn shipped() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prepare")
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    if from.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        if to.is_file() && fs::read(from)? == fs::read(to)? {
            return Ok(());
        }
        fs::copy(from, to)?;
        return Ok(());
    }
    if !from.is_dir() {
        return Err(format!("missing prepare sources {}", from.display()).into());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name() == ".local" {
            continue;
        }
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn prepare(args: &GenerateArgs) -> Result<PathBuf, Error> {
    let root = cache_root();
    eprintln!(
        "preparing worldgen resources into {} from the installed Minecraft jars",
        root.display()
    );
    copy_tree(&shipped(), &root)?;
    let options = horizon_prepare::Options {
        root: root.clone(),
        import: true,
        minecraft_dir: args.minecraft_dir.clone(),
        server_jar: args.server_jar.clone(),
        java_home: args.java_home.clone(),
    };
    horizon_prepare::prepare_generator(&options, &|message| {
        eprintln!("brahma: {message}");
    })
    .map_err(|e| e.into())
}
