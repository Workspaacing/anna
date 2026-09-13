//! Biome, as a language server Wu installs and manages itself.
//!
//! Biome is a linter and formatter for the JavaScript family, written in Rust, that covers ground
//! ESLint and Prettier otherwise split between them. Running it through its LSP rather than by
//! shelling out per file is what makes it behave like everything else in the editor: diagnostics
//! arrive on the buffer, `source.fixAll.biome` becomes available to the formatter chain, and the
//! project's own `biome.json` governs both.
//!
//! It is also the only route that gives corrected text *and* diagnostics without touching disk.
//! Biome's `--stdin-file-path` CLI mode silently disables `--reporter` entirely — it emits the fixed
//! source and nothing else, not even for a file that fails to parse — so a per-file CLI integration
//! cannot report what it could not fix.
//!
//! The project's own copy wins over the one Wu installs: that is the version its lockfile pins and
//! its CI runs, and a checker that disagrees with CI is worse than no checker.

use anyhow::Result;
use async_trait::async_trait;
use collections::HashMap;
use gpui::AsyncApp;
use language::{LanguageName, LspAdapter, LspAdapterDelegate, LspInstaller, Toolchain};
use lsp::{CodeActionKind, LanguageServerBinary, LanguageServerName, Uri};
use node_runtime::{NodeRuntime, VersionStrategy};
use project::lsp_store::language_server_settings;
use semver::Version;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};
use util::{ResultExt as _, maybe};

/// `bin/biome` is a Node shim that picks the right platform binary out of Biome's optional
/// dependencies, so it is run under Node like any other npm-delivered server. The path is the same
/// on every platform; `node_modules/.bin/biome` is not, being a shell script on Windows.
const SERVER_PATH: &str = "node_modules/@biomejs/biome/bin/biome";

fn server_binary_arguments(server_path: &Path) -> Vec<OsString> {
    vec![server_path.into(), "lsp-proxy".into()]
}

pub struct BiomeLspAdapter {
    node: NodeRuntime,
}

impl BiomeLspAdapter {
    const SERVER_NAME: LanguageServerName = LanguageServerName::new_static("biome");
    const PACKAGE_NAME: &str = "@biomejs/biome";

    /// The version every Wu installs.
    ///
    /// Pinned rather than "latest" so the version is a fact about this repository: two people on
    /// the same commit run the same linter and get the same diagnostics, and a Biome release
    /// cannot change what Wu does to your code without a commit here saying so. The same reason
    /// `eslint.rs` pins its server.
    const VERSION: &str = "2.5.13";

    pub fn new(node: NodeRuntime) -> Self {
        Self { node }
    }
}

impl LspInstaller for BiomeLspAdapter {
    type BinaryVersion = Version;

    async fn fetch_latest_server_version(
        &self,
        _: &Arc<dyn LspAdapterDelegate>,
        _: bool,
        _: &mut AsyncApp,
    ) -> Result<Self::BinaryVersion> {
        Ok(Self::VERSION.parse()?)
    }

    async fn check_if_user_installed(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &AsyncApp,
    ) -> Option<LanguageServerBinary> {
        // The project's own dependency first: it is pinned by the lockfile and is what CI runs.
        if let Ok(Some((node_modules, _version))) = delegate
            .npm_package_installed_version(Self::PACKAGE_NAME)
            .await
        {
            let server_path = node_modules.join(Self::PACKAGE_NAME).join("bin/biome");
            return Some(LanguageServerBinary {
                path: self.node.binary_path().await.ok()?,
                env: None,
                arguments: server_binary_arguments(&server_path),
            });
        }

        // Then a standalone `biome` the user put on their PATH, which needs no Node at all.
        let path = delegate.which(Self::SERVER_NAME.as_ref()).await?;
        let env = delegate.shell_env().await;
        Some(LanguageServerBinary {
            path,
            env: Some(env),
            arguments: vec!["lsp-proxy".into()],
        })
    }

    fn fetch_server_binary(
        &self,
        _latest_version: Self::BinaryVersion,
        container_dir: PathBuf,
        _: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<LanguageServerBinary>> + use<> {
        let node = self.node.clone();

        async move {
            let server_path = container_dir.join(SERVER_PATH);

            node.npm_install_packages(&container_dir, &[(Self::PACKAGE_NAME, Self::VERSION)])
                .await?;

            Ok(LanguageServerBinary {
                path: node.binary_path().await?,
                env: None,
                arguments: server_binary_arguments(&server_path),
            })
        }
    }

    fn check_if_version_installed(
        &self,
        version: &Self::BinaryVersion,
        container_dir: &PathBuf,
        _: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Option<LanguageServerBinary>> + use<> {
        let node = self.node.clone();
        let version = version.clone();
        let container_dir = container_dir.clone();

        async move {
            let server_path = container_dir.join(SERVER_PATH);

            let should_install = node
                .should_install_npm_package(
                    Self::PACKAGE_NAME,
                    &server_path,
                    &container_dir,
                    VersionStrategy::Pin(&version),
                )
                .await;

            if should_install {
                None
            } else {
                Some(LanguageServerBinary {
                    path: node.binary_path().await.ok()?,
                    env: None,
                    arguments: server_binary_arguments(&server_path),
                })
            }
        }
    }

    async fn cached_server_binary(
        &self,
        container_dir: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        get_cached_server_binary(container_dir, &self.node).await
    }
}

#[async_trait(?Send)]
impl LspAdapter for BiomeLspAdapter {
    fn name(&self) -> LanguageServerName {
        Self::SERVER_NAME
    }

    /// What the formatter chain may ask Biome to do.
    ///
    /// `source.fixAll.biome` is the one that matters: it applies every safe lint fix, which is what
    /// makes `"formatter": [{"code_actions": {"source.fixAll.biome": true}}, ...]` work. Import
    /// sorting is separate because it reorders code and some projects want it only on demand.
    fn code_action_kinds(&self) -> Option<Vec<CodeActionKind>> {
        Some(vec![
            CodeActionKind::QUICKFIX,
            CodeActionKind::new("source.fixAll.biome"),
            CodeActionKind::new("source.organizeImports.biome"),
        ])
    }

    async fn workspace_configuration(
        self: Arc<Self>,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: Option<Uri>,
        cx: &mut AsyncApp,
    ) -> Result<Value> {
        // Biome reads `biome.json` itself; anything here is the user overriding that from Wu's
        // settings, so it is passed through untouched rather than merged with opinions.
        let settings = cx.update(|cx| {
            language_server_settings(delegate.as_ref(), &Self::SERVER_NAME, cx)
                .and_then(|settings| settings.settings.clone())
                .unwrap_or_else(|| json!({}))
        });
        Ok(json!({ "biome": settings }))
    }

    /// The language ids Biome expects, which are VS Code's rather than Wu's.
    fn language_ids(&self) -> HashMap<LanguageName, String> {
        HashMap::from_iter([
            (
                LanguageName::new_static("JavaScript"),
                "javascript".to_owned(),
            ),
            (
                LanguageName::new_static("TypeScript"),
                "typescript".to_owned(),
            ),
            (
                LanguageName::new_static("TSX"),
                "typescriptreact".to_owned(),
            ),
            (LanguageName::new_static("JSON"), "json".to_owned()),
            (LanguageName::new_static("JSONC"), "jsonc".to_owned()),
            (LanguageName::new_static("CSS"), "css".to_owned()),
            (LanguageName::new_static("GraphQL"), "graphql".to_owned()),
            (LanguageName::new_static("Vue.js"), "vue".to_owned()),
            (LanguageName::new_static("Svelte"), "svelte".to_owned()),
            (LanguageName::new_static("Astro"), "astro".to_owned()),
        ])
    }
}

async fn get_cached_server_binary(
    container_dir: PathBuf,
    node: &NodeRuntime,
) -> Option<LanguageServerBinary> {
    maybe!(async {
        let server_path = container_dir.join(SERVER_PATH);
        anyhow::ensure!(
            server_path.exists(),
            "missing executable in directory {server_path:?}"
        );
        Ok(LanguageServerBinary {
            path: node.binary_path().await?,
            env: None,
            arguments: server_binary_arguments(&server_path),
        })
    })
    .await
    .log_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_a_fact_about_this_repository() {
        // Pinned rather than "latest", so two people on the same commit run the same linter. A
        // version that will not parse would install nothing at all, silently.
        let version: Version = BiomeLspAdapter::VERSION
            .parse()
            .expect("the pinned version must be valid semver");

        assert!(version.major >= 2, "Biome 1.x spells its config differently");
    }

    #[test]
    fn the_server_is_started_as_an_lsp_proxy() {
        let arguments = server_binary_arguments(Path::new("/somewhere/bin/biome"));

        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[1], OsString::from("lsp-proxy"));
    }

    #[test]
    fn the_shim_is_the_entry_point_rather_than_the_bin_directory() {
        // `node_modules/.bin/biome` is a shell script on Windows and cannot be spawned there; the
        // package's own `bin/biome` is a Node shim that works everywhere.
        assert!(SERVER_PATH.ends_with("@biomejs/biome/bin/biome"), "{SERVER_PATH}");
        assert!(!SERVER_PATH.contains(".bin"), "{SERVER_PATH}");
    }

    #[test]
    fn the_fix_all_action_is_offered_to_the_formatter_chain() {
        let kinds = BiomeLspAdapter::new(NodeRuntime::unavailable())
            .code_action_kinds()
            .expect("biome must advertise its code actions");

        assert!(kinds.contains(&CodeActionKind::new("source.fixAll.biome")));
        assert!(kinds.contains(&CodeActionKind::new("source.organizeImports.biome")));
    }
}
