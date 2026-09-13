use client::{APP_URL_SCHEME, LEGACY_APP_URL_SCHEME};
use gpui::{AsyncApp, actions};
use util::ResultExt as _;

actions!(
    cli,
    [
        /// Registers Anna as the handler for anna:// links and for the link scheme
        /// earlier releases used.
        #[action(
            name = "RegisterAnnaScheme",
            deprecated_aliases = ["cli::RegisterWuScheme"]
        )]
        RegisterWuScheme
    ]
);

/// Registers the app for its URL scheme and for the scheme earlier releases
/// registered, so links written before the rename keep opening the app.
///
/// Only a failure for the current scheme is returned; the legacy scheme is
/// best effort and its failure is logged.
pub async fn register_wu_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.register_url_scheme(APP_URL_SCHEME))
        .await?;
    cx.update(|cx| cx.register_url_scheme(LEGACY_APP_URL_SCHEME))
        .await
        .log_err();
    Ok(())
}
