// SPDX-License-Identifier: MPL-2.0
use crate::list::list_plugins_in_home;
use crate::manifest::{InstalledPlugin, PluginManifest, load_manifest_file};
use crate::paths::manifest_path;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Version selector used during compile-time plugin resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginVersionSelector {
    /// Resolve the highest installed SemVer version.
    Latest,
    /// Resolve a specific installed version string.
    Exact(String),
}

/// Resolves one installed plugin version from an explicit Transist home.
///
/// Fails when zero or more than one version is installed. Use
/// [`resolve_plugin_with_selector_in_home`] when explicit version selection is
/// required.
pub fn resolve_plugin_in_home(
    home_dir: &Path,
    plugin_id: &str,
) -> Result<(PluginManifest, PathBuf)> {
    let matches: Vec<_> = list_plugins_in_home(home_dir)?
        .into_iter()
        .filter(|plugin| plugin.id == plugin_id)
        .collect();
    match matches.as_slice() {
        [] => bail!("plugin '{plugin_id}' is not installed"),
        [single] => Ok((
            load_manifest_file(&manifest_path(&single.dir))?,
            single.dir.clone(),
        )),
        _ => bail!(
            "plugin '{plugin_id}' has multiple installed versions; remove extras or pin a single version"
        ),
    }
}

/// Resolves a plugin by id and version selector from an explicit Transist home.
///
/// - [`PluginVersionSelector::Latest`] picks the highest SemVer among all
///   installed versions. Any non-SemVer installed version string is treated as
///   a hard error so that ordering is unambiguous.
/// - [`PluginVersionSelector::Exact`] looks up the specific installed version
///   string and fails when it is not present.
pub fn resolve_plugin_with_selector_in_home(
    home_dir: &Path,
    plugin_id: &str,
    selector: &PluginVersionSelector,
) -> Result<(PluginManifest, PathBuf)> {
    if let PluginVersionSelector::Exact(version) = selector {
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::PluginId,
            plugin_id,
        )?;
        transist_protocol::validate_identifier(
            transist_protocol::IdentifierKind::PluginVersion,
            version,
        )?;
        let dir = home_dir.join("plugins").join(plugin_id).join(version);
        let manifest = load_manifest_file(&manifest_path(&dir))?;
        if manifest.id != plugin_id || &manifest.version != version {
            bail!("plugin_pin_mismatch: installed identity differs from requested pin");
        }
        return Ok((manifest, dir));
    }
    let matches: Vec<InstalledPlugin> = list_plugins_in_home(home_dir)?
        .into_iter()
        .filter(|plugin| plugin.id == plugin_id)
        .collect();
    if matches.is_empty() {
        bail!("plugin '{plugin_id}' is not installed");
    }
    match selector {
        PluginVersionSelector::Exact(version) => {
            let found = matches
                .iter()
                .find(|p| &p.version == version)
                .ok_or_else(|| {
                    anyhow::anyhow!("plugin '{plugin_id}' version '{version}' is not installed")
                })?;
            Ok((
                load_manifest_file(&manifest_path(&found.dir))?,
                found.dir.clone(),
            ))
        }
        PluginVersionSelector::Latest => {
            let mut versioned: Vec<(semver::Version, &InstalledPlugin)> = Vec::new();
            let mut non_semver: Vec<String> = Vec::new();
            for plugin in &matches {
                match semver::Version::parse(&plugin.version) {
                    Ok(v) => versioned.push((v, plugin)),
                    Err(_) => non_semver.push(plugin.version.clone()),
                }
            }
            if !non_semver.is_empty() {
                bail!(
                    "plugin '{}' has non-SemVer installed versions that cannot be ordered for 'latest' resolution: {}",
                    plugin_id,
                    non_semver.join(", ")
                );
            }
            let (_, best) = versioned
                .into_iter()
                .max_by(|(va, _), (vb, _)| va.cmp(vb))
                .ok_or_else(|| anyhow::anyhow!("plugin '{plugin_id}' is not installed"))?;
            Ok((
                load_manifest_file(&manifest_path(&best.dir))?,
                best.dir.clone(),
            ))
        }
    }
}
