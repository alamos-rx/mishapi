//! Importers: convert external formats (OpenAPI, Postman, cURL) into collections and requests.

pub mod curl;
pub mod openapi;
pub mod postman;

use crate::{
    domain::model::{Collection, Request},
    i18n::t,
};

pub enum Imported {
    Request(Request),
    Collection(Collection),
}

/// Detects the format and converts it. `url` = where it was downloaded from, to resolve a relative server
/// of the contract ("/api/v3" -> "https://host/api/v3"). Also returns the format name.
pub fn detect(text: &str, url: Option<&str>) -> Result<(&'static str, Imported), String> {
    // JSON exported on Windows sometimes has a BOM, which serde rejects.
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err(t("no hay nada que importar", "there is nothing to import").into());
    }
    if text.starts_with("curl") {
        return curl::parse_curl(text).map(|r| ("cURL", Imported::Request(r)));
    }
    let spec = openapi::parse(text).map_err(|_| {
        t(
            "no se reconoce el formato: usa OpenAPI 3 / Swagger 2 (YAML o JSON), una colección de Postman v2 o un comando cURL",
            "unrecognized format: use OpenAPI 3 / Swagger 2 (YAML or JSON), a Postman v2 collection or a cURL command",
        )
    })?;
    if !openapi::is_openapi(&spec) {
        return postman::import_postman(text).map(|c| ("Postman", Imported::Collection(c)));
    }
    let mut c = openapi::to_collection(&spec, "API")?;
    if let (Some(base), Some(origin)) = (c.variables.get_mut("baseUrl"), url.and_then(origin_of)) {
        if base.starts_with('/') {
            *base = format!("{origin}{base}");
        }
    }
    Ok(("OpenAPI", Imported::Collection(c)))
}

/// "https://h:8080/a/b" -> "https://h:8080".
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats_and_resolves_relative_servers() {
        let spec = "openapi: 3.0.0\ninfo: {title: Shop}\nservers: [{url: /api/v3}]\npaths:\n  /pets:\n    get: {summary: List}\n";
        let Ok(("OpenAPI", Imported::Collection(c))) = detect(spec, Some("https://petstore.io/api/v3/openapi.yaml")) else { panic!() };
        assert_eq!((c.name.as_str(), c.variables["baseUrl"].as_str(), c.requests.len()), ("Shop", "https://petstore.io/api/v3", 1));
        assert!(matches!(detect("\u{feff}curl https://x", None), Ok(("cURL", Imported::Request(_)))));
        assert!(matches!(detect(r#"{"info":{"name":"P"},"item":[]}"#, None), Ok(("Postman", Imported::Collection(_)))));
        assert!(detect("  ", None).is_err());
    }
}
