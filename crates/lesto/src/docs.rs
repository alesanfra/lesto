//! Interactive documentation pages (Swagger UI and Scalar).
//!
//! The pages load their scripts from jsDelivr, pinned to an exact version and guarded by a
//! subresource integrity hash, so a new upstream release cannot change what your `/docs`
//! serves. Self-hosted or mirrored assets go through [`crate::App::scalar_script_url`] and
//! [`crate::App::swagger_ui_base_url`].

/// Pinned `@scalar/api-reference` release: `SCALAR_VERSION` and the SRI hash of its entry point.
pub const SCALAR_VERSION: &str = "1.68.0";
const SCALAR_INTEGRITY: &str =
    "sha384-ayGz8N+NChlUEfR0zr5Zy3T6Q4lhcdiASJNoshS6+vxV56ZE300qfWNBjj9pqsLN";

/// Pinned `swagger-ui-dist` release and the SRI hashes of the two files the page loads.
pub const SWAGGER_UI_VERSION: &str = "5.32.15";
const SWAGGER_UI_CSS_INTEGRITY: &str =
    "sha384-fgyWYkUAamzuI8mJFu/xpRP0JWCJRwkwUwsYDoOYVHUJ8NQE5cENn8ib3ppwFFSX";
const SWAGGER_UI_JS_INTEGRITY: &str =
    "sha384-m7zaGj7MPzU+G4lz2eyy73GxK9bbRDr9bB2CSdj8wodg2wu/Wnt6wsoLP3JD+RS9";

/// Where the documentation pages load their assets from. `None` means the pinned CDN default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsAssets {
    /// URL of the Scalar script.
    pub scalar_script: Option<String>,
    /// Directory holding `swagger-ui.css` and `swagger-ui-bundle.js`.
    pub swagger_ui_base: Option<String>,
}

/// `(url, integrity attribute or empty)` for the Scalar script.
fn scalar_script(assets: &DocsAssets) -> (String, String) {
    match &assets.scalar_script {
        Some(url) => (url.clone(), String::new()),
        None => (
            format!("https://cdn.jsdelivr.net/npm/@scalar/api-reference@{SCALAR_VERSION}"),
            integrity(SCALAR_INTEGRITY),
        ),
    }
}

/// `(css url, css integrity, js url, js integrity)` for Swagger UI.
fn swagger_ui_files(assets: &DocsAssets) -> (String, String, String, String) {
    match &assets.swagger_ui_base {
        Some(base) => {
            let base = base.trim_end_matches('/');
            (
                format!("{base}/swagger-ui.css"),
                String::new(),
                format!("{base}/swagger-ui-bundle.js"),
                String::new(),
            )
        }
        None => {
            let base = format!("https://cdn.jsdelivr.net/npm/swagger-ui-dist@{SWAGGER_UI_VERSION}");
            (
                format!("{base}/swagger-ui.css"),
                integrity(SWAGGER_UI_CSS_INTEGRITY),
                format!("{base}/swagger-ui-bundle.js"),
                integrity(SWAGGER_UI_JS_INTEGRITY),
            )
        }
    }
}

fn integrity(hash: &str) -> String {
    format!(r#" integrity="{hash}" crossorigin="anonymous""#)
}

/// The URL a page at `page` uses to reach `target`, relative when both are paths.
///
/// `/docs` → `/openapi.json` gives `openapi.json`, so the pages keep working when the whole
/// app is mounted under a prefix the app does not know about (an API Gateway stage, a reverse
/// proxy). A `target` with a scheme is returned unchanged.
pub fn relative_url(page: &str, target: &str) -> String {
    if target.contains("://") || !target.starts_with('/') || !page.starts_with('/') {
        return target.to_string();
    }
    let page_dirs: Vec<&str> = {
        let mut v: Vec<&str> = page.split('/').filter(|s| !s.is_empty()).collect();
        v.pop(); // the page itself
        v
    };
    let target_segments: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let common = page_dirs
        .iter()
        .zip(&target_segments)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = vec![".."; page_dirs.len() - common];
    out.extend(&target_segments[common..]);
    if out.is_empty() {
        return ".".to_string();
    }
    out.join("/")
}

/// Minimal HTML escaping for text that lands in an attribute or a text node.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn swagger_ui_html(openapi_url: &str, title: &str, assets: &DocsAssets) -> String {
    let (css, css_integrity, js, js_integrity) = swagger_ui_files(assets);
    let title = escape(title);
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title} - Swagger UI</title>
  <link rel="stylesheet" href="{css}"{css_integrity}>
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="{js}"{js_integrity}></script>
  <script>
    window.ui = SwaggerUIBundle({{
      url: "{openapi_url}",
      dom_id: "#swagger-ui",
      deepLinking: true,
      displayRequestDuration: true,
      layout: "BaseLayout",
    }});
  </script>
</body>
</html>"##
    )
}

pub fn scalar_html(openapi_url: &str, title: &str, assets: &DocsAssets) -> String {
    let (js, js_integrity) = scalar_script(assets);
    let title = escape(title);
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title} - Scalar</title>
</head>
<body>
  <div id="app"></div>
  <script src="{js}"{js_integrity}></script>
  <script>
    Scalar.createApiReference('#app', {{ url: "{openapi_url}" }});
  </script>
</body>
</html>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_urls() {
        assert_eq!(relative_url("/docs", "/openapi.json"), "openapi.json");
        assert_eq!(
            relative_url("/api/docs", "/openapi.json"),
            "../openapi.json"
        );
        assert_eq!(relative_url("/docs", "/spec/v1.json"), "spec/v1.json");
        assert_eq!(
            relative_url("/api/v1/docs", "/api/spec.json"),
            "../spec.json"
        );
        assert_eq!(
            relative_url("/docs", "https://x.test/openapi.json"),
            "https://x.test/openapi.json"
        );
    }

    #[test]
    fn titles_are_escaped() {
        let html = scalar_html("openapi.json", "A <b> & \"C\"", &DocsAssets::default());
        assert!(html.contains("<title>A &lt;b&gt; &amp; &quot;C&quot; - Scalar</title>"));
    }
}
