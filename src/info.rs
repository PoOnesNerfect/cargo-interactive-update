/// Implementation from cargo-info command source code.
/// Used to get package info from index and is slower than using
/// cargo-search, but it does not require credentials from private
/// registries. https://github.com/rust-lang/cargo/blob/master/src/cargo/ops/registry/info/mod.rs
use std::collections::HashSet;

use anyhow::bail;
use cargo::{
    core::{
        registry::PackageRegistry, Package, PackageId, PackageIdSpec,
        PackageIdSpecQuery, Registry, SourceId, Workspace,
    },
    ops,
    ops::RegistryOrIndex,
    sources::{
        source::{QueryKind, Source},
        IndexSummary, SourceConfigMap,
    },
    util::{cache_lock::CacheLockMode, command_prelude::root_manifest},
    CargoResult, GlobalContext,
};
use cargo_util_schemas::core::PartialVersion;
use semver::Version;

use crate::{cargo::CargoDependency, dependency::Dependency};

pub fn fetch_package_from_source(
    dep: CargoDependency,
    workspace_member: Option<String>,
    workspace_path: Option<String>,
) -> CargoResult<Option<Dependency>> {
    let CargoDependency {
        name,
        package,
        version,
        source,
        ..
    } = dep;

    let mut gctx = GlobalContext::default()?;
    gctx.configure(0, true, None, false, false, false, &None, &[], &[])?;

    let spec = PackageIdSpec::parse(&format!(
        "{}#{}",
        source.as_ref().unwrap(),
        package
    ))?;

    let Some((package, latest_version)) = info(&gctx, &spec)? else {
        return Ok(None);
    };

    Ok(Some(Dependency {
        name,
        current_version: version.clone(),
        latest_version: latest_version.to_string(),
        repository: package.manifest().metadata().repository.clone(),
        description: package
            .manifest()
            .metadata()
            .description
            .as_ref()
            .map(|d| d.lines().next().unwrap().to_owned()),
        kind: dep.kind,
        workspace_member,
        workspace_path,
        latest_version_date: None,
        current_version_date: None,
    }))
}

pub fn info(
    gctx: &GlobalContext,
    spec: &PackageIdSpec,
) -> CargoResult<Option<(Package, Version)>> {
    let mut registry = PackageRegistry::new_with_source_config(
        gctx,
        SourceConfigMap::new(gctx)?,
    )?;
    // Make sure we get the lock before we download anything.
    let _lock =
        gctx.acquire_package_cache_lock(CacheLockMode::DownloadExclusive)?;
    registry.lock_patches();

    // If we can find it in workspace, use it as a specific version.
    let nearest_manifest_path = root_manifest(None, gctx).ok();
    let ws = nearest_manifest_path
        .as_ref()
        .and_then(|root| Workspace::new(root, gctx).ok());
    validate_locked_and_frozen_options(ws.is_some(), gctx)?;
    let nearest_package = ws.as_ref().and_then(|ws| {
        nearest_manifest_path
            .as_ref()
            .and_then(|path| ws.members().find(|p| p.manifest_path() == path))
    });

    let (mut package_id, _is_member) =
        find_pkgid_in_ws(nearest_package, ws.as_ref(), spec);
    let (use_package_source_id, source_ids) =
        get_source_id(gctx, None, package_id)?;
    // If we don't use the package's source, we need to query the package ID
    // from the specified registry.
    if !use_package_source_id {
        package_id = None;
    }

    let msrv_from_nearest_manifest_path_or_ws =
        try_get_msrv_from_nearest_manifest_or_ws(nearest_package, ws.as_ref());
    // If the workspace does not have a specific Rust version,
    // or if the command is not called within the workspace, then fallback to
    // the global Rust version.
    let rustc_version = match msrv_from_nearest_manifest_path_or_ws {
        Some(msrv) => msrv,
        None => {
            let current_rustc = gctx.load_global_rustc(ws.as_ref())?.version;
            // Remove any pre-release identifiers for easier comparison.
            // Otherwise, the MSRV check will fail if the current Rust version
            // is a nightly or beta version.
            semver::Version::new(
                current_rustc.major,
                current_rustc.minor,
                current_rustc.patch,
            )
            .into()
        }
    };
    // // Only suggest cargo tree command when the package is not a workspace
    // member. // For workspace members, `cargo tree --package <SPEC>
    // --invert` is useless. It only prints itself.
    // let suggest_cargo_tree_command = package_id.is_some() && !is_member;

    let summaries = query_summaries(spec, &mut registry, &source_ids)?;
    let package_id = match package_id {
        Some(id) => id,
        None => find_pkgid_in_summaries(
            &summaries,
            spec,
            &rustc_version,
            &source_ids,
        )?,
    };

    let package_set = registry.get(&[package_id])?;
    let package = package_set.get_one(package_id)?.clone();

    // summary has information about max version of this package
    let summary = summaries
        .iter()
        .max_by_key(|s| s.as_summary().version())
        .map(|e| e.as_summary());

    Ok(summary.map(|s| (package, s.version().clone())))
}

fn find_pkgid_in_ws(
    nearest_package: Option<&Package>,
    ws: Option<&cargo::core::Workspace<'_>>,
    spec: &PackageIdSpec,
) -> (Option<PackageId>, bool) {
    let Some(ws) = ws else {
        return (None, false);
    };

    if let Some(member) = ws.members().find(|p| spec.matches(p.package_id())) {
        return (Some(member.package_id()), true);
    }

    let Ok((_, resolve)) = ops::resolve_ws(ws, true) else {
        return (None, false);
    };

    if let Some(package_id) = nearest_package
        .map(|p| p.package_id())
        .into_iter()
        .flat_map(|p| resolve.deps(p))
        .map(|(p, _)| p)
        .filter(|&p| spec.matches(p))
        .max_by_key(|&p| p.version())
    {
        return (Some(package_id), false);
    }

    if let Some(package_id) = ws
        .members()
        .map(|p| p.package_id())
        .flat_map(|p| resolve.deps(p))
        .map(|(p, _)| p)
        .filter(|&p| spec.matches(p))
        .max_by_key(|&p| p.version())
    {
        return (Some(package_id), false);
    }

    if let Some(package_id) = resolve
        .iter()
        .filter(|&p| spec.matches(p))
        .max_by_key(|&p| p.version())
    {
        return (Some(package_id), false);
    }

    (None, false)
}

fn find_pkgid_in_summaries(
    summaries: &[IndexSummary],
    spec: &PackageIdSpec,
    rustc_version: &PartialVersion,
    source_ids: &RegistrySourceIds,
) -> CargoResult<PackageId> {
    let summary = summaries
        .iter()
        .filter(|s| spec.matches(s.package_id()))
        .max_by(|s1, s2| {
            // Check the MSRV compatibility.
            let s1_matches = s1
                .as_summary()
                .rust_version()
                .map(|v| v.is_compatible_with(rustc_version))
                .unwrap_or_else(|| false);
            let s2_matches = s2
                .as_summary()
                .rust_version()
                .map(|v| v.is_compatible_with(rustc_version))
                .unwrap_or_else(|| false);
            // MSRV compatible version is preferred.
            match (s1_matches, s2_matches) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                // If both summaries match the current Rust version or neither
                // do, try to pick the latest version.
                _ => s1.package_id().version().cmp(s2.package_id().version()),
            }
        });

    match summary {
        Some(summary) => Ok(summary.package_id()),
        None => {
            anyhow::bail!(
                "could not find `{}` in registry `{}`",
                spec,
                source_ids.original.url()
            )
        }
    }
}

fn query_summaries(
    spec: &PackageIdSpec,
    registry: &mut PackageRegistry,
    source_ids: &RegistrySourceIds,
) -> CargoResult<Vec<IndexSummary>> {
    // Query without version requirement to get all index summaries.
    let dep =
        cargo::core::Dependency::parse(spec.name(), None, source_ids.original)?;
    loop {
        // Exact to avoid returning all for path/git
        match registry.query_vec(&dep, QueryKind::Exact) {
            std::task::Poll::Ready(res) => {
                break res;
            }
            std::task::Poll::Pending => registry.block_until_ready()?,
        }
    }
}

#[allow(dead_code)]
struct RegistrySourceIds {
    /// Use when looking up the auth token, or writing out `Cargo.lock`
    original: SourceId,
    /// Use when interacting with the source (querying / publishing , etc)
    ///
    /// The source for crates.io may be replaced by a built-in source for
    /// accessing crates.io with the sparse protocol, or a source for the
    /// testing framework (when the replace_crates_io function is used)
    ///
    /// User-defined source replacement is not applied.
    /// Note: This will be utilized when interfacing with the registry API.
    replacement: SourceId,
}

fn get_source_id(
    gctx: &GlobalContext,
    reg_or_index: Option<RegistryOrIndex>,
    package_id: Option<PackageId>,
) -> CargoResult<(bool, RegistrySourceIds)> {
    let (use_package_source_id, sid) = match (&reg_or_index, package_id) {
        (None, Some(package_id)) => (true, package_id.source_id()),
        (None, None) => (false, SourceId::crates_io(gctx)?),
        (Some(RegistryOrIndex::Index(url)), None) => {
            (false, SourceId::for_registry(url)?)
        }
        (Some(RegistryOrIndex::Registry(r)), None) => {
            (false, SourceId::alt_registry(gctx, r)?)
        }
        (Some(reg_or_index), Some(package_id)) => {
            let sid = match reg_or_index {
                RegistryOrIndex::Index(url) => SourceId::for_registry(url)?,
                RegistryOrIndex::Registry(r) => {
                    SourceId::alt_registry(gctx, r)?
                }
            };
            let package_source_id = package_id.source_id();
            // Same registry, use the package's source.
            if sid == package_source_id {
                (true, sid)
            } else {
                let pkg_source_replacement_sid = SourceConfigMap::new(gctx)?
                    .load(package_source_id, &HashSet::new())?
                    .replaced_source_id();
                // Use the package's source if the specified registry is a
                // replacement for the package's source.
                if pkg_source_replacement_sid == sid {
                    (true, package_source_id)
                } else {
                    (false, sid)
                }
            }
        }
    };
    // Load source replacements that are built-in to Cargo.
    let builtin_replacement_sid = SourceConfigMap::empty(gctx)?
        .load(sid, &HashSet::new())?
        .replaced_source_id();
    let replacement_sid = SourceConfigMap::new(gctx)?
        .load(sid, &HashSet::new())?
        .replaced_source_id();
    // Check if the user has configured source-replacement for the registry we
    // are querying.
    if reg_or_index.is_none() && replacement_sid != builtin_replacement_sid {
        // Neither --registry nor --index was passed and the user has configured
        // source-replacement.
        if let Some(replacement_name) = replacement_sid.alt_registry_key() {
            bail!(
                "crates-io is replaced with remote registry \
                 {replacement_name};\ninclude `--registry {replacement_name}` \
                 or `--registry crates-io`"
            );
        } else {
            bail!(
                "crates-io is replaced with non-remote-registry source \
                 {replacement_sid};\ninclude `--registry crates-io` to use \
                 crates.io"
            );
        }
    } else {
        Ok((use_package_source_id, RegistrySourceIds {
            original: sid,
            replacement: builtin_replacement_sid,
        }))
    }
}

fn validate_locked_and_frozen_options(
    in_workspace: bool,
    gctx: &GlobalContext,
) -> Result<(), anyhow::Error> {
    // Only in workspace, we can use --frozen or --locked.
    if !in_workspace {
        if gctx.locked() {
            anyhow::bail!(
                "the option `--locked` can only be used within a workspace"
            );
        }

        if gctx.frozen() {
            anyhow::bail!(
                "the option `--frozen` can only be used within a workspace"
            );
        }
    }
    Ok(())
}

fn try_get_msrv_from_nearest_manifest_or_ws(
    nearest_package: Option<&Package>,
    ws: Option<&Workspace>,
) -> Option<PartialVersion> {
    // Try to get the MSRV from the nearest manifest.
    let rust_version =
        nearest_package.and_then(|p| p.rust_version().map(|v| v.as_partial()));
    // If the nearest manifest does not have a specific Rust version, try to get
    // it from the workspace.
    rust_version
        .or_else(|| {
            ws.and_then(|ws| ws.lowest_rust_version().map(|v| v.as_partial()))
        })
        .cloned()
}
