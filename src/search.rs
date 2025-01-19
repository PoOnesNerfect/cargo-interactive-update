/// Implementation from cargo-search command source code.
/// Used to get package info from index and is faster than using
/// cargo-info, but it requires explicit credentials for private
/// registries. https://github.com/rust-lang/cargo/blob/master/src/cargo/ops/registry/info/mod.rs
use std::collections::HashSet;
use std::task::Poll;

use anyhow::{bail, format_err, Context as _};
use cargo::{
    core::SourceId,
    ops::RegistryOrIndex,
    sources::{source::Source, RegistrySource, SourceConfigMap},
    util::{auth, cache_lock::CacheLockMode, network::http::http_handle},
    CargoResult, GlobalContext,
};
use cargo_credential::Operation;
use crates_io::{Crate, Registry};
use semver::Version;

use crate::{cargo::CargoDependency, dependency::Dependency};

#[allow(dead_code)]
pub fn fetch_package_from_index(
    dep: CargoDependency,
    workspace_member: Option<String>,
    workspace_path: Option<String>,
) -> CargoResult<Option<Dependency>> {
    let CargoDependency {
        name,
        package,
        version,
        kind,
        source,
        ..
    } = dep;

    let mut gctx = GlobalContext::default()?;
    gctx.configure(0, true, None, false, false, false, &None, &[], &[])?;

    let ret = search_one(
        &package,
        &gctx,
        Some(RegistryOrIndex::Index(
            source.unwrap().split_once('+').unwrap().1.parse()?,
        )),
    )?;

    let Some(ret) = ret else {
        return Ok(None);
    };

    let parsed_current_version = Version::parse(&version)?;
    let parsed_latest_version = Version::parse(&ret.max_version)?;

    if parsed_current_version < parsed_latest_version {
        Ok(Some(Dependency {
            name,
            current_version: version,
            latest_version: ret.max_version,
            repository: None,
            description: ret
                .description
                .map(|d| d.lines().next().unwrap().to_owned()),
            kind,
            workspace_member,
            workspace_path,
            current_version_date: None,
            latest_version_date: None,
        }))
    } else {
        Ok(None)
    }
}

pub fn search_one(
    query: &str,
    gctx: &GlobalContext,
    reg_or_index: Option<RegistryOrIndex>,
) -> CargoResult<Option<Crate>> {
    let source_ids = get_source_id(gctx, reg_or_index.as_ref())?;
    let (mut registry, _) = registry(gctx, &source_ids, false)?;
    let (crates, _total_crates) =
        registry.search(query, 1).with_context(|| {
            format!(
                "failed to retrieve search results from the registry at {}",
                registry.host()
            )
        })?;

    Ok(crates.into_iter().next())

    // let names = crates
    //     .iter()
    //     .map(|krate| format!("{} = \"{}\"", krate.name, krate.max_version))
    //     .collect::<Vec<String>>();

    // let description_margin = names.iter().map(|s|
    // s.len()).max().unwrap_or_default() + 4;

    // let description_length = cmp::max(80, 128 - description_margin);

    // let descriptions = crates.iter().map(|krate| {
    //     krate
    //         .description
    //         .as_ref()
    //         .map(|desc| truncate_with_ellipsis(&desc.replace("\n", " "),
    // description_length)) });

    // for (name, description) in names.into_iter().zip(descriptions) {
    //     let line = match description {
    //         Some(desc) => format!("{name: <description_margin$}# {desc}"),
    //         None => name,
    //     };
    //     let mut fragments = line.split(query).peekable();
    //     while let Some(fragment) = fragments.next() {
    //         let _ = write!(stdout, "{fragment}");
    //         if fragments.peek().is_some() {
    //             let _ = write!(stdout, "{good}{query}{good:#}");
    //         }
    //     }
    //     let _ = writeln!(stdout);
    // }

    // let search_max_limit = 100;
    // if total_crates > limit && limit < search_max_limit {
    //     let _ = writeln!(
    //         stdout,
    //         "... and {} crates more (use --limit N to see more)",
    //         total_crates - limit
    //     );
    // } else if total_crates > limit && limit >= search_max_limit {
    //     let extra = if source_ids.original.is_crates_io() {
    //         let url = Url::parse_with_params("https://crates.io/search", &[("q", query)])?;
    //         format!(" (go to {url} to see more)")
    //     } else {
    //         String::new()
    //     };
    //     let _ = writeln!(
    //         stdout,
    //         "... and {} crates more{}",
    //         total_crates - limit,
    //         extra
    //     );
    // }

    // if total_crates > 0 {
    //     let literal = LITERAL;
    //     shell.note(format_args!(
    //         "to learn more about a package, run `{literal}cargo info
    // <name>{literal:#}`",     ))?;
    // }

    // Ok(())
}

/// Returns the `Registry` and `Source` based on command-line and config
/// settings.
///
/// * `source_ids`: The source IDs for the registry. It contains the original
///   source ID and the replacement source ID.
/// * `index`: The index URL from the command-line.
/// * `registry`: The registry name from the command-line. If neither
///   `registry`, or `index` are set, then uses `crates-io`.
/// * `force_update`: If `true`, forces the index to be updated.
fn registry<'gctx>(
    gctx: &'gctx GlobalContext,
    source_ids: &RegistrySourceIds,
    force_update: bool,
) -> CargoResult<(Registry, RegistrySource<'gctx>)> {
    let mut src =
        RegistrySource::remote(source_ids.replacement, &HashSet::new(), gctx)?;
    let cfg = {
        let _lock =
            gctx.acquire_package_cache_lock(CacheLockMode::DownloadExclusive)?;
        // Only update the index if `force_update` is set.
        if force_update {
            src.invalidate_cache()
        }
        let cfg = loop {
            match src.config()? {
                Poll::Pending => {
                    src.block_until_ready().with_context(|| {
                        format!("failed to update {}", source_ids.replacement)
                    })?
                }
                Poll::Ready(cfg) => break cfg,
            }
        };
        cfg.expect("remote registries must have config")
    };
    let api_host = cfg.api.ok_or_else(|| {
        format_err!("{} does not support API commands", source_ids.replacement)
    })?;
    let token = if cfg.auth_required {
        Some(auth::auth_token(
            gctx,
            &source_ids.original,
            None,
            Operation::Read,
            vec![],
            false,
        )?)
    } else {
        None
    };
    let handle = http_handle(gctx)?;
    Ok((
        Registry::new_handle(api_host, token, handle, cfg.auth_required),
        src,
    ))
}

pub(crate) struct RegistrySourceIds {
    /// Use when looking up the auth token, or writing out `Cargo.lock`
    pub(crate) original: SourceId,
    /// Use when interacting with the source (querying / publishing , etc)
    ///
    /// The source for crates.io may be replaced by a built-in source for
    /// accessing crates.io with the sparse protocol, or a source for the
    /// testing framework (when the `replace_crates_io` function is used)
    ///
    /// User-defined source replacement is not applied.
    pub(crate) replacement: SourceId,
}

fn get_initial_source_id(
    gctx: &GlobalContext,
    reg_or_index: Option<&RegistryOrIndex>,
) -> CargoResult<SourceId> {
    match reg_or_index {
        None => SourceId::crates_io(gctx),
        Some(reg_or_index) => {
            get_initial_source_id_from_registry_or_index(gctx, reg_or_index)
        }
    }
}

fn get_initial_source_id_from_registry_or_index(
    gctx: &GlobalContext,
    reg_or_index: &RegistryOrIndex,
) -> CargoResult<SourceId> {
    match reg_or_index {
        RegistryOrIndex::Index(url) => SourceId::for_registry(url),
        RegistryOrIndex::Registry(r) => SourceId::alt_registry(gctx, r),
    }
}

fn get_replacement_source_ids(
    gctx: &GlobalContext,
    sid: SourceId,
) -> CargoResult<(SourceId, SourceId)> {
    let builtin_replacement_sid = SourceConfigMap::empty(gctx)?
        .load(sid, &HashSet::new())?
        .replaced_source_id();
    let replacement_sid = SourceConfigMap::new(gctx)?
        .load(sid, &HashSet::new())?
        .replaced_source_id();
    Ok((builtin_replacement_sid, replacement_sid))
}

/// Gets the `SourceId` for an index or registry setting.
///
/// The `index` and `reg` values are from the command-line or config settings.
/// If both are None, and no source-replacement is configured, returns the
/// source for crates.io. If both are None, and source replacement is
/// configured, returns an error.
///
/// The source for crates.io may be GitHub, index.crates.io, or a test-only
/// registry depending on configuration.
///
/// If `reg` is set, source replacement is not followed.
///
/// The return value is a pair of `SourceId`s: The first may be a built-in
/// replacement of crates.io (such as index.crates.io), while the second is
/// always the original source.
pub(crate) fn get_source_id(
    gctx: &GlobalContext,
    reg_or_index: Option<&RegistryOrIndex>,
) -> CargoResult<RegistrySourceIds> {
    let sid = get_initial_source_id(gctx, reg_or_index)?;
    let (builtin_replacement_sid, replacement_sid) =
        get_replacement_source_ids(gctx, sid)?;

    if reg_or_index.is_none() && replacement_sid != builtin_replacement_sid {
        bail!(gen_replacement_error(replacement_sid));
    } else {
        Ok(RegistrySourceIds {
            original: sid,
            replacement: builtin_replacement_sid,
        })
    }
}

fn gen_replacement_error(replacement_sid: SourceId) -> String {
    // Neither --registry nor --index was passed and the user has configured
    // source-replacement.
    let error_message =
        if let Some(replacement_name) = replacement_sid.alt_registry_key() {
            format!(
                "crates-io is replaced with remote registry {};\ninclude \
                 `--registry {}` or `--registry crates-io`",
                replacement_name, replacement_name
            )
        } else {
            format!(
                "crates-io is replaced with non-remote-registry source \
                 {};\ninclude `--registry crates-io` to use crates.io",
                replacement_sid
            )
        };

    error_message
}
