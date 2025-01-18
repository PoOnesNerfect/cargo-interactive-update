use std::{cmp::Ordering, env::current_dir};

use cargo_lock::Lockfile;
use semver::{Version, VersionReq};
use toml_edit::{DocumentMut, Item, Value};

use crate::dependency::{Dependencies, Dependency, DependencyKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoDependency {
    pub name: String,
    pub package: String,
    pub version: String,
    pub version_req: VersionReq,
    pub kind: DependencyKind,
    pub path: Option<String>,
    pub source: Option<String>,
}

impl Ord for CargoDependency {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let ordering = self.kind.cmp(&other.kind);

        if ordering == std::cmp::Ordering::Equal {
            self.name.cmp(&other.name)
        } else {
            ordering
        }
    }
}

impl PartialOrd for CargoDependency {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub struct CargoDependencies {
    dependencies: Vec<CargoDependency>,
    pub cargo_toml: DocumentMut,
}

impl CargoDependencies {
    pub fn gather_dependencies() -> Self {
        let cargo_toml = read_cargo_file();

        let dependencies = get_cargo_dependencies(&cargo_toml);

        Self {
            dependencies,
            cargo_toml,
        }
    }

    pub fn into_parts(self, loader: crate::loading::Loader) -> (Dependencies, DocumentMut) {
        let mut local_deps = Vec::new();
        let mut crates_io_deps = Vec::new();
        let mut alt_registry_deps = Vec::new();

        for dep in self.dependencies {
            if dep.path.is_some() {
                local_deps.push(dep);
            } else {
                let source = dep
                    .source
                    .as_ref()
                    .expect("source must exist for non-path deps");

                if source == "registry+https://github.com/rust-lang/crates.io-index" {
                    crates_io_deps.push(dep);
                } else {
                    alt_registry_deps.push(dep);
                }
            }
        }

        let crates_io_threads = crates_io_deps
            .into_iter()
            .map(|d| {
                let loader = loader.clone();
                std::thread::spawn(move || {
                    let ret = crate::api::fetch_package_from_crates_io(d);
                    loader.inc_loader();
                    ret
                })
            })
            .collect::<Vec<_>>();

        let alt_threads = alt_registry_deps
            .into_iter()
            .map(|d| {
                let loader = loader.clone();
                std::thread::spawn(move || {
                    let ret = crate::info::fetch_package_from_source(d);
                    loader.inc_loader();
                    ret
                })
            })
            .collect::<Vec<_>>();

        let mut deps = local_deps
            .into_iter()
            .filter_map(|d| {
                let ret = get_latest_version_from_path(d);
                loader.inc_loader();
                ret
            })
            .chain(
                crates_io_threads
                    .into_iter()
                    .filter_map(|t| t.join().map(|e| e).ok().flatten()),
            )
            .chain(
                alt_threads
                    .into_iter()
                    .filter_map(|t| t.join().map(|e| e.ok()).ok().flatten().flatten()),
            )
            .collect::<Vec<_>>();
        deps.sort();

        (Dependencies::new(deps), self.cargo_toml)
    }

    pub fn len(&self) -> usize {
        self.dependencies.len()
    }
}

fn read_cargo_file() -> DocumentMut {
    let cargo_toml_content =
        std::fs::read_to_string("Cargo.toml").expect("Unable to read Cargo.toml file");

    cargo_toml_content
        .parse()
        .expect("Unable to parse Cargo.toml file as TOML")
}

fn get_cargo_dependencies(cargo_toml: &DocumentMut) -> Vec<CargoDependency> {
    let lockfile = read_cargo_lock_file();

    let dependencies = extract_dependencies_from_sections(
        cargo_toml.get("dependencies"),
        DependencyKind::Normal,
        &lockfile,
    );

    let dev_dependencies = extract_dependencies_from_sections(
        cargo_toml.get("dev-dependencies"),
        DependencyKind::Dev,
        &lockfile,
    );

    let build_dependencies = extract_dependencies_from_sections(
        cargo_toml.get("build-dependencies"),
        DependencyKind::Build,
        &lockfile,
    );

    let workspace_dependencies = extract_dependencies_from_sections(
        cargo_toml
            .get("workspace")
            .and_then(|w| w.get("dependencies")),
        DependencyKind::Workspace,
        &lockfile,
    );

    dependencies
        .into_iter()
        .chain(dev_dependencies)
        .chain(build_dependencies)
        .chain(workspace_dependencies)
        .collect()
}

fn read_cargo_lock_file() -> Lockfile {
    let mut dir = current_dir().unwrap();

    // try recursing parents 7 times to find lockfile
    for _ in 0..7 {
        let path = dir.join("Cargo.lock");

        if let Ok(lockfile) = Lockfile::load(path) {
            return lockfile;
        }
        dir = if let Some(parent) = dir.parent() {
            parent.to_path_buf()
        } else {
            panic!("Unable to read Cargo.lock file");
        };
    }

    panic!("Unable to read Cargo.lock file");
}

fn extract_dependencies_from_sections(
    cargo_toml: Option<&Item>,
    kind: DependencyKind,
    lockfile: &Lockfile,
) -> Vec<CargoDependency> {
    let Some(cargo_toml) = cargo_toml else {
        return vec![];
    };

    let Some(package_deps) = cargo_toml.as_table_like() else {
        return vec![];
    };

    package_deps
        .iter()
        .flat_map(|(name, package_data)| {
            let (package_name, version_req, path) = match package_data {
                Item::Value(Value::String(version)) => {
                    (None, version.value().as_str().to_owned(), None)
                }
                Item::Value(Value::InlineTable(t)) => {
                    let is_workspace = t
                        .get("workspace")
                        .map(|e| e.as_bool())
                        .flatten()
                        .unwrap_or(false);

                    // if dependency is from workspace, then skip
                    if is_workspace {
                        return None;
                    }

                    (
                        t.get("package")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned()),
                        t.get("version")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned())
                            .expect("version field must exist"),
                        t.get("path")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned()),
                    )
                }
                Item::Table(t) => {
                    let is_workspace = t
                        .get("workspace")
                        .map(|e| e.as_bool())
                        .flatten()
                        .unwrap_or(false);

                    // if dependency is from workspace, then skip
                    if is_workspace {
                        return None;
                    }

                    (
                        t.get("package")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned()),
                        t.get("version")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned())
                            .expect("version field must exist"),
                        t.get("path")
                            .map(|e| e.as_str())
                            .flatten()
                            .map(|e| e.to_owned()),
                    )
                }
                _ => return None,
            };

            let version_req =
                VersionReq::parse(&version_req).expect("must be a valid version requirement");

            let package_name = package_name.as_ref().map(|e| e.as_str()).unwrap_or(name);

            let package = find_matching_package(&lockfile, package_name, &version_req);

            Some(CargoDependency {
                name: name.to_owned(),
                package: package_name.to_owned(),
                version: package.version.to_string(),
                version_req,
                kind,
                path,
                source: package.source.as_ref().map(|e| e.to_string()),
            })
        })
        .collect()
}

fn find_matching_package<'a>(
    lockfile: &'a Lockfile,
    package_name: &str,
    req: &VersionReq,
) -> &'a cargo_lock::Package {
    let packages = &lockfile.packages;

    // index of the package instance
    let i = packages
        .binary_search_by_key(&package_name, |p| p.name.as_str())
        .expect("crate should exist in lockfile");

    let package = &packages[i];
    if req.matches(&package.version) {
        return package;
    }

    // search through packages around the found index
    // to find the crate of matching version
    if i + 1 < packages.len() {
        let package_ = packages[i + 1..]
            .iter()
            .take_while(|p| p.name.as_str() == package_name)
            .find(|p| req.matches(&p.version));
        if let Some(package) = package_ {
            return package;
        }
    }

    if i > 0 {
        let package_ = packages[..i]
            .iter()
            .rev()
            .take_while(|p| p.name.as_str() == package_name)
            .find(|p| req.matches(&p.version));
        if let Some(package) = package_ {
            return package;
        }
    }

    panic!(
        "unable to find matching crate '{package_name} = {}' in Cargo.lock",
        req
    );
}

fn get_latest_version_from_path(dep: CargoDependency) -> Option<Dependency> {
    use cargo_toml::Manifest;
    use std::path::PathBuf;

    let path = dep.path.unwrap();

    let mut cargo_path = PathBuf::from(&path);
    cargo_path.push("Cargo.toml");

    let Ok(manifest) = Manifest::from_path(&cargo_path) else {
        panic!(
            "Unable to read file {}",
            cargo_path.as_os_str().to_str().unwrap()
        );
    };

    let package = manifest.package.expect("package must exist");

    let description = package
        .description()
        .map(|d| d.split_whitespace().next().unwrap().to_owned());
    let repository = package.repository().map(|d| d.to_owned());

    let parsed_current_version = Version::parse(&dep.version).ok()?;
    let parsed_latest_version = Version::parse(&package.version()).ok()?;

    if parsed_current_version < parsed_latest_version {
        Some(Dependency {
            latest_version: package.version().to_owned(),
            name: package.name,
            current_version: dep.version,
            path: Some(path),
            repository,
            description,
            kind: dep.kind,
        })
    } else {
        None
    }
}
