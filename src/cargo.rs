use std::{collections::HashMap, env::current_dir};

use cargo_lock::Lockfile;
use semver::{Version, VersionReq};
use toml_edit::{DocumentMut, Item, Value};

use crate::dependency::{Dependencies, DependencyKind};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CargoDependency {
    pub name: String,
    pub package: String,
    pub version: String,
    pub kind: DependencyKind,
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

#[derive(Debug, Clone, Default)]
pub struct CargoDependencies {
    pub cargo_toml: DocumentMut,
    package_name: Option<String>,
    dependencies: Vec<CargoDependency>,
    workspace_members: HashMap<String, Box<CargoDependencies>>,
}

impl CargoDependencies {
    pub fn gather_dependencies() -> Self {
        Self::gather_dependencies_inner(".", &read_cargo_lock_file())
    }

    fn gather_dependencies_inner(
        relative_path: &str,
        lockfile: &Lockfile,
    ) -> Self {
        let cargo_toml = read_cargo_file(relative_path);
        let package_name = get_package_name(&cargo_toml);
        let dependencies = get_cargo_dependencies(&cargo_toml, lockfile);
        let workspace_members = get_workspace_members(&cargo_toml, lockfile);

        Self {
            cargo_toml,
            package_name,
            dependencies,
            workspace_members,
        }
    }

    pub fn retrieve_outdated_dependencies(
        self,
        workspace_path: Option<String>,
        loader: crate::loading::Loader,
    ) -> Dependencies {
        let Self {
            package_name: workspace_member,
            cargo_toml,
            dependencies,
            workspace_members,
        } = self;

        let mut cargo_toml_files = HashMap::new();
        cargo_toml_files.insert(
            workspace_path.clone().unwrap_or_else(|| ".".to_string()),
            cargo_toml,
        );

        let mut crates_io_deps = Vec::new();
        let mut alt_registry_deps = Vec::new();

        for dep in dependencies {
            if let Some(source) = dep.source.as_ref() {
                if source
                    == "registry+https://github.com/rust-lang/crates.io-index"
                {
                    crates_io_deps.push(dep);
                } else {
                    alt_registry_deps.push(dep);
                }
            }
        }

        let mut ws_threads = Vec::new();
        for (member, dependencies) in workspace_members.into_iter() {
            let loader = loader.clone();
            ws_threads.push(std::thread::spawn(move || {
                dependencies
                    .retrieve_outdated_dependencies(Some(member), loader)
            }));
        }

        let crates_io_threads = crates_io_deps
            .into_iter()
            .map(|d| {
                let ws_member = workspace_member.clone();
                let ws_path = workspace_path.clone();
                let loader = loader.clone();
                std::thread::spawn(move || {
                    let ret = crate::api::fetch_package_from_crates_io(
                        d, ws_member, ws_path,
                    );
                    loader.inc_loader();
                    ret
                })
            })
            .collect::<Vec<_>>();

        let alt_threads = alt_registry_deps
            .into_iter()
            .map(|d| {
                let ws_member = workspace_member.clone();
                let ws_path = workspace_path.clone();
                let loader = loader.clone();
                std::thread::spawn(move || {
                    let ret = crate::info::fetch_package_from_source(
                        d, ws_member, ws_path,
                    );
                    loader.inc_loader();
                    ret
                })
            })
            .collect::<Vec<_>>();

        let mut deps = crates_io_threads
            .into_iter()
            .filter_map(|t| t.join().map(|e| e).ok().flatten())
            .chain(alt_threads.into_iter().filter_map(|t| {
                t.join().map(|e| e.ok()).ok().flatten().flatten()
            }))
            .filter(|e| {
                let parsed_current_version = Version::parse(&e.current_version)
                    .expect("Current version is not a valid semver");
                let parsed_latest_version = Version::parse(&e.latest_version)
                    .expect("Latest version is not a valid semver");

                parsed_current_version < parsed_latest_version
            })
            .collect::<Vec<_>>();

        ws_threads.into_iter().for_each(|h| {
            let ws = h.join().unwrap();
            deps.extend(ws.dependencies);
            cargo_toml_files.extend(ws.cargo_toml_files);
        });

        deps.sort();

        Dependencies::new(deps, cargo_toml_files)
    }

    pub fn len(&self) -> usize {
        self.dependencies.len()
            + self
                .workspace_members
                .values()
                .fold(0, |acc, deps| acc + deps.len())
    }
}

fn read_cargo_file(relative_path: &str) -> DocumentMut {
    let cargo_toml_content =
        std::fs::read_to_string(format!("{relative_path}/Cargo.toml"))
            .unwrap_or_else(|e| {
                eprintln!("Unable to read Cargo.toml file: {}", e);
                String::new()
            });

    cargo_toml_content
        .parse()
        .expect("Unable to parse Cargo.toml file as TOML")
}

fn get_cargo_dependencies(
    cargo_toml: &DocumentMut,
    lockfile: &Lockfile,
) -> Vec<CargoDependency> {
    let dependencies = extract_dependencies_from_sections(
        cargo_toml.get("dependencies"),
        DependencyKind::Normal,
        lockfile,
    );

    let dev_dependencies = extract_dependencies_from_sections(
        cargo_toml.get("dev-dependencies"),
        DependencyKind::Dev,
        lockfile,
    );

    let build_dependencies = extract_dependencies_from_sections(
        cargo_toml.get("build-dependencies"),
        DependencyKind::Build,
        lockfile,
    );

    let workspace_dependencies = extract_dependencies_from_sections(
        cargo_toml
            .get("workspace")
            .and_then(|w| w.get("dependencies")),
        DependencyKind::Workspace,
        lockfile,
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
    dependencies_section: Option<&Item>,
    kind: DependencyKind,
    lockfile: &Lockfile,
) -> Vec<CargoDependency> {
    let Some(dependencies_section) = dependencies_section else {
        return vec![];
    };

    let Some(package_deps) = dependencies_section.as_table_like() else {
        return vec![];
    };

    package_deps
        .iter()
        .flat_map(|(name, package_data)| {
            let (version_req, package) = match package_data {
                Item::Value(Value::String(v)) => (v.value().to_string(), None),
                Item::Value(Value::InlineTable(t)) => (
                    t.get("version")?.as_str()?.to_owned(),
                    t.get("package")
                        .map(|e| e.as_str().map(|e| e.to_owned()))
                        .flatten(),
                ),
                Item::Table(t) => (
                    t.get("version")?.as_str()?.to_owned(),
                    t.get("package")
                        .map(|e| e.as_str().map(|e| e.to_owned()))
                        .flatten(),
                ),
                _ => return None,
            };

            let version_req = VersionReq::parse(&version_req)
                .expect("must be a valid version requirement");

            let package_name =
                package.as_ref().map(|e| e.as_str()).unwrap_or(name);

            let package =
                find_matching_package(&lockfile, package_name, &version_req);

            Some(CargoDependency {
                name: name.to_owned(),
                package: package.name.as_str().to_owned(),
                version: package.version.to_string(),
                kind,
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
    let Ok(i) =
        packages.binary_search_by_key(&package_name, |p| p.name.as_str())
    else {
        panic!(
            "unable to find matching crate '{package_name} = \"{}\"' in \
             Cargo.lock",
            req
        );
    };

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
        "unable to find matching crate '{package_name} = \"{}\"' in Cargo.lock",
        req
    );
}

fn get_workspace_members(
    cargo_toml: &DocumentMut,
    lockfile: &Lockfile,
) -> HashMap<String, Box<CargoDependencies>> {
    let Some(workspace_members) = cargo_toml
        .get("workspace")
        .and_then(|i| i.get("members"))
        .and_then(|i| i.as_array())
    else {
        return HashMap::new();
    };

    workspace_members
        .iter()
        .fold(HashMap::new(), |mut acc, member| {
            let Some(member) = member.as_str() else {
                return acc;
            };

            acc.insert(
                member.to_string(),
                Box::new(CargoDependencies::gather_dependencies_inner(
                    member, lockfile,
                )),
            );
            acc
        })
}

fn get_package_name(cargo_toml: &DocumentMut) -> Option<String> {
    cargo_toml
        .get("package")
        .and_then(|i| i.get("name"))
        .and_then(|i| i.as_str().map(|e| e.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_cargo_dependencies_len() {
        let cargo_dependencies = CargoDependencies {
            dependencies: vec![Default::default()],
            workspace_members: HashMap::from_iter([(
                "".to_string(),
                Box::new(CargoDependencies {
                    dependencies: vec![Default::default()],
                    ..Default::default()
                }),
            )]),
            ..Default::default()
        };
        assert_eq!(cargo_dependencies.len(), 2);
    }

    #[test]
    fn test_get_cargo_dependencies() {
        const CARGO_TOML: &str = r#"
        [dependencies]
        "dependencies" = "^0.1.0"

        [dev-dependencies]
        "dev-dependencies" = "=1.0.0"

        [build-dependencies]
        "build-dependencies" = "^2.0.0"

        [workspace.dependencies]
        "workspace-dependencies" = "^3.0.0"
        "#;

        const CARGO_LOCK: &str = r#"
        version = 4

        [[package]]
        name = "build-dependencies"
        version = "2.1.0"

        [[package]]
        name = "dependencies"
        version = "0.1.2"

        [[package]]
        name = "dev-dependencies"
        version = "1.0.0"

        [[package]]
        name = "workspace-dependencies"
        version = "3.0.0"
        "#;

        let cargo_toml: DocumentMut = CARGO_TOML.parse().unwrap();
        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();
        let dependencies = get_cargo_dependencies(&cargo_toml, &lockfile);
        assert_eq!(dependencies.len(), 4);
        assert!(dependencies.contains(&CargoDependency {
            name: "dependencies".to_string(),
            package: "dependencies".to_string(),
            version: "0.1.2".to_string(),
            kind: DependencyKind::Normal,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "dev-dependencies".to_string(),
            package: "dev-dependencies".to_string(),
            version: "1.0.0".to_string(),
            kind: DependencyKind::Dev,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "build-dependencies".to_string(),
            package: "build-dependencies".to_string(),
            version: "2.1.0".to_string(),
            kind: DependencyKind::Build,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "workspace-dependencies".to_string(),
            package: "workspace-dependencies".to_string(),
            version: "3.0.0".to_string(),
            kind: DependencyKind::Workspace,
            source: None
        }));
    }

    #[test]
    fn test_extract_dependencies_from_sections() {
        const CARGO_TOML: &str = r#"
        [dependencies]
        "cargo-outdated" = "0.1.0"
        "other-dependency" = { version = "1.0.0" }
        "random-dependency" = { version = "2.0.0", package = "other-name" }
        "invalid-dependency" = 123

        [dependencies.serde]
        version = "1.0.0"
        "#;

        const CARGO_LOCK: &str = r#"
        version = 4

        [[package]]
        name = "cargo-outdated"
        version = "0.1.0"

        [[package]]
        name = "other-dependency"
        version = "1.0.0"

        [[package]]
        name = "other-name"
        version = "2.0.0"

        [[package]]
        name = "serde"
        version = "1.0.0"
        "#;

        let cargo_toml: DocumentMut = CARGO_TOML.parse().unwrap();
        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();

        let dependencies = extract_dependencies_from_sections(
            cargo_toml.get("dependencies"),
            DependencyKind::Normal,
            &lockfile,
        );

        assert_eq!(dependencies.len(), 4);
        assert!(dependencies.contains(&CargoDependency {
            name: "cargo-outdated".to_string(),
            package: "cargo-outdated".to_string(),
            version: "0.1.0".to_string(),
            kind: DependencyKind::Normal,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "other-dependency".to_string(),
            package: "other-dependency".to_string(),
            version: "1.0.0".to_string(),
            kind: DependencyKind::Normal,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "random-dependency".to_string(),
            package: "other-name".to_string(),
            version: "2.0.0".to_string(),
            kind: DependencyKind::Normal,
            source: None
        }));
        assert!(dependencies.contains(&CargoDependency {
            name: "serde".to_string(),
            package: "serde".to_string(),
            version: "1.0.0".to_string(),
            kind: DependencyKind::Normal,
            source: None
        }));
    }

    #[test]
    fn test_extract_dependencies_with_none_dependencies_section() {
        const CARGO_LOCK: &str = r#"
        version = 4
        "#;

        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();
        let dependencies = extract_dependencies_from_sections(
            None,
            DependencyKind::Normal,
            &lockfile,
        );
        assert_eq!(dependencies.len(), 0);
    }

    #[test]
    fn test_extract_dependencies_with_dependencies_section_not_a_table() {
        const CARGO_LOCK: &str = r#"
        version = 4
        "#;

        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();

        let dependencies = extract_dependencies_from_sections(
            Some(&Item::Value(Value::from(false))),
            DependencyKind::Normal,
            &lockfile,
        );
        assert_eq!(dependencies.len(), 0);
    }

    #[test]
    fn test_get_workspace_members() {
        const CARGO_TOML: &str = r#"
        [workspace]
        members = ["workspace-member-1", "workspace-member-2", 0]
        "#;

        const CARGO_LOCK: &str = r#"
        version = 4
        "#;

        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();

        let cargo_toml = CARGO_TOML.parse().unwrap();
        let workspace_members = get_workspace_members(&cargo_toml, &lockfile);
        assert_eq!(workspace_members.len(), 2);
        assert!(workspace_members.contains_key("workspace-member-1"));
        assert!(workspace_members.contains_key("workspace-member-2"));
    }

    #[test]
    fn test_get_workspace_members_with_no_workspace() {
        const CARGO_TOML: &str = r#"
        [dependencies]
        "cargo-outdated" = "0.1.0"
        "#;

        const CARGO_LOCK: &str = r#"
        version = 4

        [[package]]
        name = "cargo-outdated"
        version = "0.1.0"
        "#;

        let cargo_toml = CARGO_TOML.parse().unwrap();
        let lockfile = Lockfile::from_str(CARGO_LOCK).unwrap();
        let workspace_members = get_workspace_members(&cargo_toml, &lockfile);
        assert_eq!(workspace_members.len(), 0);
    }

    #[test]
    fn test_get_package_name_with_no_package() {
        const CARGO_TOML: &str = r#"
        [dependencies]
        "cargo-outdated" = "0.1.0"
        "#;

        let cargo_toml = CARGO_TOML.parse().unwrap();
        let package_name = get_package_name(&cargo_toml);
        assert_eq!(package_name, None);
    }

    #[test]
    fn test_get_package_name() {
        const CARGO_TOML: &str = r#"
        [package]
        name = "cargo-outdated"
        "#;

        let cargo_toml = CARGO_TOML.parse().unwrap();
        let package_name = get_package_name(&cargo_toml);
        assert_eq!(package_name.unwrap(), "cargo-outdated");
    }
}
