use depsy_lsp::cache::{MemoryCache, WriteCache};
use depsy_lsp::file_types::FileType;
use depsy_lsp::parsers::Parser;
use depsy_lsp::parsers::lockfile_resolver::{resolve_versions_from_lockfile, select_resolver};
use depsy_lsp::parsers::npm::NpmParser;
use depsy_lsp::parsers::pnpm_workspace::{
    PnpmWorkspaceParser, read_pnpm_workspace_for_package, resolve_catalog_references,
    resolve_catalog_versions_from_lockfile,
};
use depsy_lsp::providers::code_actions::create_code_actions;
use depsy_lsp::registries::VersionInfo;
use tower_lsp::lsp_types::{CodeActionOrCommand, Position, Range, TextEdit, Url};

fn dependency_pairs(content: &str) -> Vec<(String, String)> {
    let mut pairs = PnpmWorkspaceParser::new()
        .parse(content)
        .into_iter()
        .map(|dependency| (dependency.name, dependency.version))
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs
}

#[test]
fn default_catalog_entries_accept_inline_comments_and_quoted_versions() {
    let workspace_yaml = r#"
packages: [packages/*]
catalog: # shared dependency versions
  "@types/node": "^20.0.0"
  git-dep: "github:example/repo#v1.2.3" # pinned git ref
  react: "^18.3.1" # pinned for React 18 apps
  redux: '^5.0.1'
  unquoted-git: github:example/repo#v1.2.3 # plain scalar keeps git ref
"#;

    assert_eq!(
        dependency_pairs(workspace_yaml),
        vec![
            ("@types/node".to_string(), "^20.0.0".to_string()),
            (
                "git-dep".to_string(),
                "github:example/repo#v1.2.3".to_string(),
            ),
            ("react".to_string(), "^18.3.1".to_string()),
            ("redux".to_string(), "^5.0.1".to_string()),
            (
                "unquoted-git".to_string(),
                "github:example/repo#v1.2.3".to_string(),
            ),
        ]
    );
}

#[test]
fn default_catalog_shapes_determine_discovered_npm_dependencies() {
    // Given a workspace file "pnpm-workspace.yaml" represented as compact YAML "<workspace_yaml>"
    // When Depsy inspects "pnpm-workspace.yaml"
    // Then the discovered npm dependencies are "<expected_dependencies>"
    let cases = [
        (
            "packages: [packages/*]\ncatalog:\n  react: ^18.3.1\n  redux: ^5.0.1\n",
            vec![
                ("react".to_string(), "^18.3.1".to_string()),
                ("redux".to_string(), "^5.0.1".to_string()),
            ],
        ),
        (
            "packages: [packages/*]\ncatalog: { \"@types/node\": ^20.0.0, git-dep: github:example/repo#v1.2.3, react: ^18.3.1, redux: ^5.0.1 }\n",
            vec![
                ("@types/node".to_string(), "^20.0.0".to_string()),
                (
                    "git-dep".to_string(),
                    "github:example/repo#v1.2.3".to_string(),
                ),
                ("react".to_string(), "^18.3.1".to_string()),
                ("redux".to_string(), "^5.0.1".to_string()),
            ],
        ),
        ("packages: [packages/*]\ncatalog: {}\n", Vec::new()),
        ("packages: [packages/*]\n", Vec::new()),
    ];

    for (workspace_yaml, expected_dependencies) in cases {
        assert_eq!(dependency_pairs(workspace_yaml), expected_dependencies);
    }
}

#[test]
fn named_catalog_shapes_determine_discovered_npm_dependencies() {
    // Given a workspace file "pnpm-workspace.yaml" represented as compact YAML "<workspace_yaml>"
    // When Depsy inspects "pnpm-workspace.yaml"
    // Then the discovered npm dependencies are "<expected_dependencies>"
    let cases = [
        (
            "packages: [packages/*]\ncatalogs:\n  react18:\n    react: ^18.2.0\n    react-dom: ^18.2.0\n",
            vec![
                ("react".to_string(), "^18.2.0".to_string()),
                ("react-dom".to_string(), "^18.2.0".to_string()),
            ],
        ),
        (
            "packages: [packages/*]\ncatalogs: { react18: { react: ^18.2.0, react-dom: ^18.2.0 } }\n",
            vec![
                ("react".to_string(), "^18.2.0".to_string()),
                ("react-dom".to_string(), "^18.2.0".to_string()),
            ],
        ),
        (
            "packages: [packages/*]\ncatalogs:\n  react18: { react: ^18.2.0, react-dom: ^18.2.0 }\n",
            vec![
                ("react".to_string(), "^18.2.0".to_string()),
                ("react-dom".to_string(), "^18.2.0".to_string()),
            ],
        ),
        (
            "packages: [packages/*]\ncatalogs:\n  react18: {}\n",
            Vec::new(),
        ),
        ("packages: [packages/*]\ncatalogs: {}\n", Vec::new()),
    ];

    for (workspace_yaml, expected_dependencies) in cases {
        assert_eq!(dependency_pairs(workspace_yaml), expected_dependencies);
    }
}

#[test]
fn react_catalog_shorthand_resolves_through_the_default_catalog() {
    // Given a workspace file "pnpm-workspace.yaml" containing:
    //   packages:
    //     - packages/*
    //
    //   catalog:
    //     react: ^18.3.1
    //     redux: ^5.0.1
    // And a package file "packages/example-app/package.json" containing:
    //   {
    //     "name": "@example/app",
    //     "dependencies": {
    //       "react": "catalog:"
    //     }
    //   }
    // When Depsy inspects "packages/example-app/package.json"
    // Then the dependency "react" resolves to npm version range "^18.3.1"
    let workspace_yaml = r#"
packages:
  - packages/*

catalog:
  react: ^18.3.1
  redux: ^5.0.1
"#;
    let package_json = r#"{
  "name": "@example/app",
  "dependencies": {
    "react": "catalog:"
  }
}"#;

    let dependencies =
        resolve_catalog_references(NpmParser::new().parse(package_json), Some(workspace_yaml));

    let react = dependencies
        .iter()
        .find(|dependency| dependency.name == "react")
        .unwrap();
    assert_eq!(react.version, "catalog:");
    assert_eq!(react.resolved_version.as_deref(), Some("^18.3.1"));
    assert_eq!(react.effective_version(), "^18.3.1");
}

#[test]
fn catalog_shorthand_ignores_named_catalog_entries_without_default_catalog() {
    let workspace_yaml = r#"
packages:
  - packages/*

catalogs:
  react18:
    react: ^18.2.0
"#;
    let package_json = r#"{
  "name": "@example/app",
  "dependencies": {
    "react": "catalog:"
  }
}"#;

    let dependencies =
        resolve_catalog_references(NpmParser::new().parse(package_json), Some(workspace_yaml));

    let react = dependencies
        .iter()
        .find(|dependency| dependency.name == "react")
        .unwrap();
    assert_eq!(react.version, "catalog:");
    assert_eq!(react.resolved_version, None);
}

#[test]
fn react_dom_named_catalog_reference_resolves_through_react18() {
    // Given a workspace file "pnpm-workspace.yaml" containing:
    //   packages:
    //     - packages/*
    //
    //   catalogs:
    //     react18:
    //       react: ^18.2.0
    //       react-dom: ^18.2.0
    // And a package file "packages/example-components/package.json" containing:
    //   {
    //     "name": "@example/components",
    //     "dependencies": {
    //       "react-dom": "catalog:react18"
    //     }
    //   }
    // When Depsy inspects "packages/example-components/package.json"
    // Then the dependency "react-dom" resolves to npm version range "^18.2.0"
    let workspace_yaml = r#"
packages:
  - packages/*

catalogs:
  react18:
    react: ^18.2.0
    react-dom: ^18.2.0
"#;
    let package_json = r#"{
  "name": "@example/components",
  "dependencies": {
    "react-dom": "catalog:react18"
  }
}"#;

    let dependencies =
        resolve_catalog_references(NpmParser::new().parse(package_json), Some(workspace_yaml));

    let react_dom = dependencies
        .iter()
        .find(|dependency| dependency.name == "react-dom")
        .unwrap();
    assert_eq!(react_dom.version, "catalog:react18");
    assert_eq!(react_dom.resolved_version.as_deref(), Some("^18.2.0"));
    assert_eq!(react_dom.effective_version(), "^18.2.0");
}

#[test]
fn quoted_named_catalog_reference_resolves_through_unquoted_catalog_name() {
    let workspace_yaml = r#"
packages:
  - packages/*

catalogs:
  "react18":
    react: ^18.2.0
"#;
    let package_json = r#"{
  "name": "@example/app",
  "dependencies": {
    "react": "catalog:react18"
  }
}"#;

    let dependencies =
        resolve_catalog_references(NpmParser::new().parse(package_json), Some(workspace_yaml));

    let react = dependencies
        .iter()
        .find(|dependency| dependency.name == "react")
        .unwrap();
    assert_eq!(react.version, "catalog:react18");
    assert_eq!(react.resolved_version.as_deref(), Some("^18.2.0"));
}

#[tokio::test]
async fn package_json_catalog_resolution_reads_nearest_workspace_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace_path = tmp.path().join("pnpm-workspace.yaml");
    let package_dir = tmp.path().join("packages").join("example-app");
    let package_path = package_dir.join("package.json");

    std::fs::create_dir_all(&package_dir).expect("create package dir");
    std::fs::write(
        &workspace_path,
        "packages: [packages/*]\ncatalog: { react: ^18.3.1 }\n",
    )
    .expect("write workspace");
    std::fs::write(
        &package_path,
        r#"{
  "dependencies": {
    "react": "catalog:"
  }
}"#,
    )
    .expect("write package");

    let package_json = std::fs::read_to_string(&package_path).expect("read package");
    let workspace_content = read_pnpm_workspace_for_package(&package_path).await;
    let dependencies = resolve_catalog_references(
        NpmParser::new().parse(&package_json),
        workspace_content.as_deref(),
    );

    let react = dependencies
        .iter()
        .find(|dependency| dependency.name == "react")
        .unwrap();
    assert_eq!(react.version, "catalog:");
    assert_eq!(react.resolved_version.as_deref(), Some("^18.3.1"));
}

#[test]
fn workspace_file_entries_expose_editable_name_and_version_spans() {
    // Given a workspace file "pnpm-workspace.yaml" opened directly in the editor
    // When Depsy inspects its catalogs
    // Then each entry points at the name and version text on its own line
    let workspace_yaml = r#"packages:
  - packages/*
catalog:
  "@types/node": "^20.0.0"
  lodash: ^4.17.21 # utility belt
catalogs:
  react18:
    react: '^18.3.1'
"#;
    let lines = workspace_yaml.lines().collect::<Vec<_>>();
    let dependencies = PnpmWorkspaceParser::new().parse(workspace_yaml);

    let spanned = dependencies
        .iter()
        .map(|dependency| {
            let name_line = lines[dependency.name_span.line as usize];
            let version_line = lines[dependency.version_span.line as usize];
            (
                &name_line[dependency.name_span.line_start as usize
                    ..dependency.name_span.line_end as usize],
                &version_line[dependency.version_span.line_start as usize
                    ..dependency.version_span.line_end as usize],
                dependency.version_span.line,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        spanned,
        vec![
            ("@types/node", "^20.0.0", 3),
            ("lodash", "^4.17.21", 4),
            ("react", "^18.3.1", 7),
        ]
    );
}

fn locked_versions(workspace_yaml: &str, lock_yaml: &str) -> Vec<(String, String, Option<String>)> {
    let mut dependencies = PnpmWorkspaceParser::new().parse(workspace_yaml);
    resolve_catalog_versions_from_lockfile(&mut dependencies, workspace_yaml, lock_yaml);
    dependencies
        .into_iter()
        .map(|dependency| {
            (
                dependency.name,
                dependency.version,
                dependency.resolved_version,
            )
        })
        .collect()
}

fn locked(name: &str, range: &str, version: Option<&str>) -> (String, String, Option<String>) {
    (
        name.to_string(),
        range.to_string(),
        version.map(str::to_string),
    )
}

#[test]
fn catalog_entries_get_the_version_locked_for_their_own_catalog() {
    // Given a workspace file whose default and "legacy" catalogs both pin "react"
    // And a lockfile that records one version of "react" per catalog
    // When Depsy resolves the catalog entries from the lockfile
    // Then each entry gets the version locked for its own catalog
    let workspace_yaml = r#"catalog:
  react: ^18.3.1
  "@types/node": ^20.0.0
catalogs:
  legacy:
    react: ^17.0.2
"#;
    let lock_yaml = r#"lockfileVersion: '9.0'

catalogs:
  default:
    '@types/node':
      specifier: ^20.0.0
      version: 20.11.5
    react:
      specifier: ^18.3.1
      version: 18.3.1
  legacy:
    react:
      specifier: ^17.0.2
      version: 17.0.2

packages:

  react@17.0.2:
    resolution: {}

  react@18.3.1:
    resolution: {}
"#;

    let resolved = locked_versions(workspace_yaml, lock_yaml);

    assert_eq!(
        resolved,
        vec![
            locked("react", "^18.3.1", Some("18.3.1")),
            locked("@types/node", "^20.0.0", Some("20.11.5")),
            locked("react", "^17.0.2", Some("17.0.2")),
        ]
    );
}

#[test]
fn unused_catalog_entries_ignore_unrelated_locked_versions_of_the_package() {
    // Given a workspace file whose catalog pins "react" at "^18.3.1"
    // And a lockfile where no project uses that entry but "react@17.0.2" is installed
    // When Depsy resolves the catalog entries from the lockfile
    // Then the entry has no locked version
    let workspace_yaml = "catalog:\n  react: ^18.3.1\n";
    let lock_yaml = r#"lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      react:
        specifier: ^17.0.2
        version: 17.0.2

packages:

  react@17.0.2:
    resolution: {}
"#;

    let resolved = locked_versions(workspace_yaml, lock_yaml);

    assert_eq!(resolved, vec![locked("react", "^18.3.1", None)]);
}

#[test]
fn catalog_entries_edited_since_the_last_install_have_no_locked_version() {
    // Given a workspace file whose catalog pins "react" at "^18.3.1"
    // And a lockfile written when the catalog pinned "react" at "^17.0.2"
    // When Depsy resolves the catalog entries from the lockfile
    // Then the entry has no locked version
    let workspace_yaml = "catalog:\n  react: ^18.3.1\n";
    let lock_yaml = r#"lockfileVersion: '9.0'

catalogs:
  default:
    react:
      specifier: ^17.0.2
      version: 17.0.2
"#;

    let resolved = locked_versions(workspace_yaml, lock_yaml);

    assert_eq!(resolved, vec![locked("react", "^18.3.1", None)]);
}

#[tokio::test]
async fn workspace_file_versions_are_resolved_from_the_pnpm_lockfile_only() {
    // Given a workspace root that holds "pnpm-workspace.yaml", "pnpm-lock.yaml"
    // and a stale "package-lock.json"
    // When Depsy resolves the catalog entries from the lockfile
    // Then the versions come from "pnpm-lock.yaml"
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace_path = tmp.path().join("pnpm-workspace.yaml");
    let workspace_yaml = "catalog:\n  lodash: ^4.17.0\n";
    std::fs::write(&workspace_path, workspace_yaml).expect("write workspace");
    std::fs::write(
        tmp.path().join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"node_modules/lodash":{"version":"4.17.15"}}}"#,
    )
    .expect("write package-lock");
    std::fs::write(
        tmp.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\ncatalogs:\n  default:\n    lodash:\n      specifier: ^4.17.0\n      version: 4.17.21\npackages:\n  lodash@4.17.21:\n    resolution: {}\n",
    )
    .expect("write pnpm-lock");
    let mut dependencies = PnpmWorkspaceParser::new().parse(workspace_yaml);

    let resolver = select_resolver(FileType::Npm, &workspace_path, workspace_yaml)
        .await
        .expect("pnpm lockfile resolver");
    resolve_versions_from_lockfile(&mut dependencies, resolver, &workspace_path).await;

    assert_eq!(dependencies[0].resolved_version.as_deref(), Some("4.17.21"));
}

#[tokio::test]
async fn workspace_file_ignores_lockfiles_of_other_package_managers() {
    // Given a workspace root that holds "pnpm-workspace.yaml" and only a "package-lock.json"
    // When Depsy looks for a lockfile for the workspace file
    // Then no lockfile is used
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace_path = tmp.path().join("pnpm-workspace.yaml");
    let workspace_yaml = "catalog:\n  lodash: ^4.17.0\n";
    std::fs::write(&workspace_path, workspace_yaml).expect("write workspace");
    std::fs::write(
        tmp.path().join("package-lock.json"),
        r#"{"lockfileVersion":3,"packages":{"node_modules/lodash":{"version":"4.17.15"}}}"#,
    )
    .expect("write package-lock");

    let resolver = select_resolver(FileType::Npm, &workspace_path, workspace_yaml).await;

    assert!(resolver.is_none());
}

#[tokio::test]
async fn update_action_rewrites_only_the_catalog_entry_version() {
    // Given a workspace file whose default and named catalogs pin outdated versions
    // And the registry reports a newer version of each package
    // When the update action of each catalog entry is applied
    // Then only that entry's version is rewritten and its range operator is kept
    let workspace_yaml = r#"packages:
  - packages/*
catalog:
  lodash: ^4.17.15 # shared
catalogs:
  legacy:
    "minimist": "1.2.0"
"#;
    let dependencies = PnpmWorkspaceParser::new().parse(workspace_yaml);
    let cache = MemoryCache::new();
    for (package, latest) in [("lodash", "4.18.1"), ("minimist", "1.2.8")] {
        cache
            .insert(
                format!("test:{package}"),
                VersionInfo {
                    latest: Some(latest.to_string()),
                    ..Default::default()
                },
            )
            .await;
    }
    let uri = Url::parse("file:///test/pnpm-workspace.yaml").expect("workspace uri");

    let mut updated = Vec::new();
    for dependency in &dependencies {
        let range = Range {
            start: Position::new(dependency.version_span.line, 0),
            end: Position::new(dependency.version_span.line, u32::MAX),
        };
        let actions = create_code_actions(
            &dependencies,
            &cache,
            &uri,
            range,
            FileType::Npm,
            |name| format!("test:{name}"),
            &[],
            None,
            None,
        )
        .await;
        let edit = actions
            .into_iter()
            .find_map(|action| {
                let CodeActionOrCommand::CodeAction(action) = action else {
                    return None;
                };
                if !action
                    .title
                    .contains(&format!("Update {} to", dependency.name))
                {
                    return None;
                }
                action.edit?.changes?.get(&uri)?.first().cloned()
            })
            .expect("update action for the catalog entry");
        updated.push(apply_text_edit(workspace_yaml, &edit));
    }

    assert_eq!(
        updated,
        vec![
            workspace_yaml.replace("^4.17.15", "^4.18.1"),
            workspace_yaml.replace("1.2.0", "1.2.8"),
        ]
    );
}

fn apply_text_edit(content: &str, edit: &TextEdit) -> String {
    let lines = content.split_inclusive('\n').collect::<Vec<_>>();
    let offset = |position: Position| {
        lines[..position.line as usize]
            .iter()
            .map(|line| line.len())
            .sum::<usize>()
            + position.character as usize
    };
    let mut updated = content.to_string();
    updated.replace_range(
        offset(edit.range.start)..offset(edit.range.end),
        &edit.new_text,
    );
    updated
}
