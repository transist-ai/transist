// SPDX-License-Identifier: MPL-2.0
use std::collections::BTreeSet;
use std::path::Path;

const CORE_OWNED_PLUGIN_FIXTURES: &[(&str, &str)] = &[("shell", "plugins/shell")];

#[test]
fn core_owned_plugins_have_valid_manifests() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (_, rel_path) in CORE_OWNED_PLUGIN_FIXTURES {
        let manifest =
            transist_plugins::load_manifest_file(&repo_root.join(rel_path).join("plugin.yaml"))
                .expect("manifest");
        assert_eq!(manifest.schema, transist_plugins::PLUGIN_MANIFEST_SCHEMA_V1);
    }
}

#[test]
fn core_owned_plugins_install_and_inspect() {
    let repo = tempfile::tempdir().expect("tempdir");
    transist::init_home_dir_with_mode(
        &transist::repo_local_home_dir(repo.path()),
        transist::RuntimeOperatingMode::Standalone,
    )
    .expect("init");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    for (_, rel_path) in CORE_OWNED_PLUGIN_FIXTURES {
        transist::TransistContext::repo_local(repo.path())
            .install_plugin_file(&repo_root.join(rel_path).join("plugin.yaml"))
            .expect("install");
    }

    let installed = transist::TransistContext::repo_local(repo.path())
        .list_plugins()
        .expect("list");
    let installed_ids = installed
        .into_iter()
        .map(|plugin| plugin.id)
        .collect::<BTreeSet<_>>();
    let expected_ids = CORE_OWNED_PLUGIN_FIXTURES
        .iter()
        .map(|(plugin_id, _)| plugin_id.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(installed_ids, expected_ids);

    for (plugin_id, _) in CORE_OWNED_PLUGIN_FIXTURES {
        let manifests = transist::TransistContext::repo_local(repo.path())
            .inspect_plugin(plugin_id)
            .expect("inspect");
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].id, *plugin_id);
    }
}

#[test]
fn directory_install_path_remains_supported() {
    let repo = tempfile::tempdir().expect("tempdir");
    transist::init_home_dir_with_mode(
        &transist::repo_local_home_dir(repo.path()),
        transist::RuntimeOperatingMode::Standalone,
    )
    .expect("init");
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    transist::TransistContext::repo_local(repo.path())
        .install_plugin_dir(&repo_root.join("plugins/shell"))
        .expect("directory install still works");
}
