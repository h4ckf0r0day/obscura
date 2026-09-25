use std::collections::HashMap;

use deno_core::ModuleSpecifier;

#[derive(Default)]
pub(crate) struct ImportMap {
    imports: SpecifierMap,
    scopes: Vec<(String, SpecifierMap)>,
    resolved_modules: Vec<ResolvedModule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedModule {
    referrer: String,
    specifier: String,
    as_url_is_special: bool,
}

#[derive(Default)]
struct SpecifierMap {
    entries: HashMap<String, Option<ModuleSpecifier>>,
    prefixes: Vec<String>,
}

impl ImportMap {
    pub(crate) fn parse(input: &str, base_url: &str) -> Result<Self, String> {
        let base = ModuleSpecifier::parse(base_url)
            .map_err(|e| format!("Invalid import map base URL {}: {}", base_url, e))?;
        let parsed: serde_json::Value =
            serde_json::from_str(input).map_err(|e| format!("Invalid import map JSON: {}", e))?;
        let object = parsed
            .as_object()
            .ok_or_else(|| "Import map top level must be an object".to_string())?;

        let imports = match object.get("imports") {
            Some(value) => SpecifierMap::parse(
                value
                    .as_object()
                    .ok_or_else(|| "Import map \"imports\" must be an object".to_string())?,
                &base,
            ),
            None => SpecifierMap::default(),
        };

        let mut scopes = Vec::new();
        if let Some(value) = object.get("scopes") {
            let scope_object = value
                .as_object()
                .ok_or_else(|| "Import map \"scopes\" must be an object".to_string())?;
            for (scope_prefix, scope_imports) in scope_object {
                // Unlike keys and addresses in a module specifier map, scope
                // prefixes use ordinary URL parsing. In particular, a bare
                // relative value such as "feature/" is valid here.
                let Ok(normalized_scope) = base.join(scope_prefix) else {
                    continue;
                };
                let scope_imports = scope_imports.as_object().ok_or_else(|| {
                    format!(
                        "Import map scope \"{}\" must contain an object",
                        scope_prefix
                    )
                })?;
                scopes.push((
                    normalized_scope.to_string(),
                    SpecifierMap::parse(scope_imports, &base),
                ));
            }
            // The most specific applicable scope is consulted first.
            scopes.sort_by(|left, right| right.0.cmp(&left.0));
        }

        // Integrity enforcement is owned by the module fetch layer. Still
        // validate the top-level shape here: a malformed integrity member
        // invalidates the complete import map in Chromium/the HTML algorithm,
        // rather than leaving its `imports` member active.
        if let Some(value) = object.get("integrity") {
            value
                .as_object()
                .ok_or_else(|| "Import map \"integrity\" must be an object".to_string())?;
        }

        Ok(Self {
            imports,
            scopes,
            resolved_modules: Vec::new(),
        })
    }

    pub(crate) fn merge(&mut self, mut new_map: Self) {
        // Multiple import maps are merged even after module graphs have
        // started. New rules which could change an already-observed
        // (referrer, specifier) resolution are removed first; unrelated rules
        // remain available to later graphs.
        for record in &self.resolved_modules {
            new_map
                .imports
                .remove_rules_affecting(&record.specifier, record.as_url_is_special);
            for (scope_prefix, scope_imports) in &mut new_map.scopes {
                if scope_applies(scope_prefix, &record.referrer) {
                    scope_imports
                        .remove_rules_affecting(&record.specifier, record.as_url_is_special);
                }
            }
        }

        self.imports.merge(new_map.imports);
        for (scope_prefix, new_imports) in new_map.scopes {
            if let Some((_, imports)) = self
                .scopes
                .iter_mut()
                .find(|(existing, _)| existing == &scope_prefix)
            {
                imports.merge(new_imports);
            } else {
                self.scopes.push((scope_prefix, new_imports));
            }
        }
        self.scopes.sort_by(|left, right| right.0.cmp(&left.0));
    }

    pub(crate) fn resolve(
        &mut self,
        specifier: &str,
        referrer: &ModuleSpecifier,
    ) -> Result<ModuleSpecifier, String> {
        let as_url = resolve_url_like(specifier, referrer);
        let normalized = as_url
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| specifier.to_string());
        let serialized_referrer = referrer.to_string();

        for (scope_prefix, scope_imports) in &self.scopes {
            if scope_applies(scope_prefix, &serialized_referrer) {
                if let Some(resolved) = scope_imports.resolve_match(&normalized, as_url.as_ref())? {
                    self.remember_resolution(serialized_referrer, normalized, as_url.as_ref());
                    return Ok(resolved);
                }
            }
        }

        if let Some(resolved) = self.imports.resolve_match(&normalized, as_url.as_ref())? {
            self.remember_resolution(serialized_referrer, normalized, as_url.as_ref());
            return Ok(resolved);
        }
        match as_url {
            Some(resolved) => {
                self.remember_resolution(serialized_referrer, normalized, Some(&resolved));
                Ok(resolved)
            }
            None => Err(format!(
                "Bare module specifier \"{}\" was not remapped by the import map",
                specifier
            )),
        }
    }

    fn remember_resolution(
        &mut self,
        referrer: String,
        specifier: String,
        as_url: Option<&ModuleSpecifier>,
    ) {
        let resolution = ResolvedModule {
            referrer,
            specifier,
            as_url_is_special: as_url.is_none_or(is_special_url),
        };
        if !self.resolved_modules.contains(&resolution) {
            self.resolved_modules.push(resolution);
        }
    }
}

impl SpecifierMap {
    fn parse(object: &serde_json::Map<String, serde_json::Value>, base: &ModuleSpecifier) -> Self {
        let mut entries = HashMap::with_capacity(object.len());
        let mut prefixes = Vec::new();
        for (key, value) in object {
            let Some(normalized_key) = normalize_key(key, base) else {
                continue;
            };
            let address = value
                .as_str()
                .and_then(|address| resolve_url_like(address, base))
                .filter(|address| !key.ends_with('/') || address.as_str().ends_with('/'));
            if normalized_key.ends_with('/') {
                prefixes.push(normalized_key.clone());
            }
            entries.insert(normalized_key, address);
        }
        prefixes.sort_by(|left, right| right.cmp(left));
        Self { entries, prefixes }
    }

    fn merge(&mut self, new_map: Self) {
        for (key, address) in new_map.entries {
            if !self.entries.contains_key(&key) {
                if key.ends_with('/') {
                    self.prefixes.push(key.clone());
                }
                self.entries.insert(key, address);
            }
        }
        self.prefixes.sort_by(|left, right| right.cmp(left));
    }

    fn remove_rules_affecting(&mut self, resolved: &str, as_url_is_special: bool) {
        self.entries.retain(|key, _| {
            key != resolved
                && !(as_url_is_special && key.ends_with('/') && resolved.starts_with(key.as_str()))
        });
        self.prefixes
            .retain(|key| self.entries.contains_key(key.as_str()));
    }

    fn resolve_match(
        &self,
        normalized: &str,
        as_url: Option<&ModuleSpecifier>,
    ) -> Result<Option<ModuleSpecifier>, String> {
        if let Some(address) = self.entries.get(normalized) {
            return address.clone().map(Some).ok_or_else(|| {
                format!(
                    "Module specifier \"{}\" is blocked by import map entry \"{}\"",
                    normalized, normalized
                )
            });
        }

        if as_url.is_some_and(|url| !is_special_url(url)) {
            return Ok(None);
        }

        for key in self
            .prefixes
            .iter()
            .filter(|key| normalized.starts_with(key.as_str()))
        {
            let address = self
                .entries
                .get(key)
                .ok_or_else(|| format!("Import map prefix \"{}\" has no matching address", key))?;
            let address = address.as_ref().ok_or_else(|| {
                format!(
                    "Module specifier \"{}\" is blocked by import map prefix \"{}\"",
                    normalized, key
                )
            })?;
            let after_prefix = &normalized[key.len()..];
            let resolved = address.join(after_prefix).map_err(|e| {
                format!(
                    "Module specifier \"{}\" could not resolve through import map prefix \"{}\": {}",
                    normalized, key, e
                )
            })?;
            if !resolved.as_str().starts_with(address.as_str()) {
                return Err(format!(
                    "Module specifier \"{}\" backtracks above import map prefix \"{}\"",
                    normalized, key
                ));
            }
            return Ok(Some(resolved));
        }
        Ok(None)
    }
}

fn normalize_key(key: &str, base: &ModuleSpecifier) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    Some(
        resolve_url_like(key, base)
            .map(|url| url.to_string())
            .unwrap_or_else(|| key.to_string()),
    )
}

fn resolve_url_like(specifier: &str, base: &ModuleSpecifier) -> Option<ModuleSpecifier> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        base.join(specifier).ok()
    } else {
        ModuleSpecifier::parse(specifier).ok()
    }
}

fn scope_applies(scope_prefix: &str, referrer: &str) -> bool {
    scope_prefix == referrer || (scope_prefix.ends_with('/') && referrer.starts_with(scope_prefix))
}

fn is_special_url(url: &ModuleSpecifier) -> bool {
    matches!(
        url.scheme(),
        "ftp" | "file" | "http" | "https" | "ws" | "wss"
    )
}

#[cfg(test)]
mod tests {
    use super::ImportMap;

    #[test]
    fn exact_prefix_and_url_like_keys_are_normalized_against_the_map_base() {
        let mut map = ImportMap::parse(
            r#"{
                "imports": {
                    "pkg": "../vendor/pkg.js",
                    "pkg/": "../vendor/pkg/",
                    "./local.js": "../vendor/local.js"
                }
            }"#,
            "https://example.test/app/maps/import-map.json",
        )
        .unwrap();
        let referrer =
            deno_core::ModuleSpecifier::parse("https://example.test/app/main.js").unwrap();

        assert_eq!(
            map.resolve("pkg", &referrer).unwrap().as_str(),
            "https://example.test/app/vendor/pkg.js",
        );
        assert_eq!(
            map.resolve("pkg/features/a.js", &referrer)
                .unwrap()
                .as_str(),
            "https://example.test/app/vendor/pkg/features/a.js",
        );
        assert_eq!(
            map.resolve("./maps/dir/../local.js", &referrer)
                .unwrap()
                .as_str(),
            "https://example.test/app/vendor/local.js",
        );
    }

    #[test]
    fn most_specific_scope_wins_then_falls_back_to_top_level_imports() {
        let mut map = ImportMap::parse(
            r#"{
                "imports": {
                    "shared": "/default.js",
                    "fallback": "/fallback.js"
                },
                "scopes": {
                    "/feature/": { "shared": "/feature.js" },
                    "/feature/nested/": { "shared": "/nested.js" }
                }
            }"#,
            "https://example.test/app/index.html",
        )
        .unwrap();

        let nested =
            deno_core::ModuleSpecifier::parse("https://example.test/feature/nested/main.js")
                .unwrap();
        assert_eq!(
            map.resolve("shared", &nested).unwrap().as_str(),
            "https://example.test/nested.js",
        );
        assert_eq!(
            map.resolve("fallback", &nested).unwrap().as_str(),
            "https://example.test/fallback.js",
        );
    }

    #[test]
    fn later_maps_add_unrelated_rules_but_cannot_change_resolved_rules() {
        let mut map = ImportMap::parse(
            r#"{"imports":{"fixed":"/first.js"}}"#,
            "https://example.test/app/index.html",
        )
        .unwrap();
        let referrer =
            deno_core::ModuleSpecifier::parse("https://example.test/app/main.js").unwrap();

        assert_eq!(
            map.resolve("fixed", &referrer).unwrap().as_str(),
            "https://example.test/first.js",
        );
        map.merge(
            ImportMap::parse(
                r#"{"imports":{"fixed":"/second.js","later":"/later.js"}}"#,
                "https://example.test/app/index.html",
            )
            .unwrap(),
        );

        assert_eq!(
            map.resolve("fixed", &referrer).unwrap().as_str(),
            "https://example.test/first.js",
        );
        assert_eq!(
            map.resolve("later", &referrer).unwrap().as_str(),
            "https://example.test/later.js",
        );
    }

    #[test]
    fn later_prefix_rules_cannot_capture_an_already_resolved_specifier() {
        let mut map = ImportMap::default();
        let referrer =
            deno_core::ModuleSpecifier::parse("https://example.test/app/main.js").unwrap();
        assert_eq!(
            map.resolve("./pkg/item.js", &referrer).unwrap().as_str(),
            "https://example.test/app/pkg/item.js",
        );

        map.merge(
            ImportMap::parse(
                r#"{"imports":{"./pkg/":"/replacement/","new/":"/new/"}}"#,
                "https://example.test/app/main.js",
            )
            .unwrap(),
        );

        assert_eq!(
            map.resolve("./pkg/item.js", &referrer).unwrap().as_str(),
            "https://example.test/app/pkg/item.js",
        );
        assert_eq!(
            map.resolve("new/item.js", &referrer).unwrap().as_str(),
            "https://example.test/new/item.js",
        );
    }

    #[test]
    fn bare_relative_scope_prefix_is_resolved_as_a_url() {
        let mut map = ImportMap::parse(
            r#"{"scopes":{"feature/":{"pkg":"/scoped.js"}}}"#,
            "https://example.test/app/index.html",
        )
        .unwrap();
        let referrer =
            deno_core::ModuleSpecifier::parse("https://example.test/app/feature/main.js").unwrap();
        assert_eq!(
            map.resolve("pkg", &referrer).unwrap().as_str(),
            "https://example.test/scoped.js",
        );
    }

    #[test]
    fn malformed_scope_or_integrity_member_invalidates_the_complete_map() {
        assert!(ImportMap::parse(
            r#"{"imports":{"pkg":"/pkg.js"},"scopes":{"/app/":[]}}"#,
            "https://example.test/index.html",
        )
        .is_err());
        assert!(ImportMap::parse(
            r#"{"imports":{"pkg":"/pkg.js"},"integrity":[]}"#,
            "https://example.test/index.html",
        )
        .is_err());
    }
}
