use crate::context::ToolContext;
use crate::resources::selector::ReadPathSelection;
use anyhow::{Result, anyhow, bail};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceContentType {
    PlainText,
    Markdown,
    Json,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceMetadata {
    pub source_label: Option<String>,
    pub total_lines: Option<usize>,
    pub total_bytes: Option<usize>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub canonical_url: String,
    pub content: String,
    pub content_type: ResourceContentType,
    pub immutable: Option<bool>,
    pub metadata: ResourceMetadata,
}

#[derive(Debug, Clone)]
pub struct ResourceRequest {
    pub resource_url: String,
    pub scheme: String,
    pub authority: String,
    pub path: String,
    pub selector: ReadPathSelection,
}

pub trait ResourceHandler: Send + Sync {
    fn scheme(&self) -> &'static str;

    fn immutable(&self) -> bool {
        true
    }

    fn resolve(&self, req: &ResourceRequest, ctx: &ToolContext) -> Result<Resource>;
}

#[derive(Default)]
pub struct ResourceRouter {
    handlers: HashMap<&'static str, Arc<dyn ResourceHandler>>,
    built_in_schemes: BTreeSet<&'static str>,
}

impl ResourceRouter {
    pub fn with_builtin_handlers() -> Self {
        Self::default()
            .with_builtin(crate::resources::artifact::ArtifactResourceHandler)
            .with_builtin(crate::resources::skill::SkillResourceHandler)
            .with_builtin(crate::resources::session::SessionResourceHandler)
    }

    pub fn with_builtin<H: ResourceHandler + 'static>(mut self, handler: H) -> Self {
        self.register(Arc::new(handler), true)
            .expect("built-in resource handler registration must be valid");
        self
    }

    pub fn register(&mut self, handler: Arc<dyn ResourceHandler>, built_in: bool) -> Result<()> {
        let scheme = handler.scheme();
        validate_scheme(scheme)?;
        if self.handlers.contains_key(scheme) {
            bail!("Error: resource handler for scheme '{scheme}' is already registered");
        }
        if built_in {
            self.built_in_schemes.insert(scheme);
        }
        self.handlers.insert(scheme, handler);
        Ok(())
    }

    #[cfg(test)]
    pub fn replace_handler_for_tests(&mut self, handler: Arc<dyn ResourceHandler>) -> Result<()> {
        let scheme = handler.scheme();
        validate_scheme(scheme)?;
        if self.built_in_schemes.contains(scheme) {
            bail!("Error: cannot replace built-in resource handler for scheme '{scheme}'");
        }
        self.handlers.insert(scheme, handler);
        Ok(())
    }

    pub fn can_handle(&self, path_without_selector: &str) -> bool {
        parse_resource_url(path_without_selector)
            .map(|parsed| self.handlers.contains_key(parsed.scheme))
            .unwrap_or(false)
    }

    pub fn is_url_like(&self, path_without_selector: &str) -> bool {
        parse_resource_url(path_without_selector).is_some()
    }

    pub fn resolve(&self, selection: &ReadPathSelection, ctx: &ToolContext) -> Result<Resource> {
        let parsed = parse_resource_url(&selection.path)
            .ok_or_else(|| anyhow!("Error: not a resource URL: {}", selection.path))?;
        let handler = self
            .handlers
            .get(parsed.scheme)
            .ok_or_else(|| anyhow!("Error: unknown resource scheme: {}", parsed.scheme))?;
        let req = ResourceRequest {
            resource_url: selection.path.clone(),
            scheme: parsed.scheme.to_string(),
            authority: parsed.authority.to_string(),
            path: parsed.path.to_string(),
            selector: selection.clone(),
        };
        let mut resource = handler.resolve(&req, ctx)?;
        if resource.immutable.is_none() {
            resource.immutable = Some(handler.immutable());
        }
        Ok(resource)
    }
}

struct ParsedResourceUrl<'a> {
    scheme: &'a str,
    authority: &'a str,
    path: &'a str,
}

fn parse_resource_url(input: &str) -> Option<ParsedResourceUrl<'_>> {
    let (scheme, rest) = input.split_once("://")?;
    if !is_valid_scheme(scheme) {
        return None;
    }
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, ""), |(authority, path)| (authority, path));
    Some(ParsedResourceUrl {
        scheme,
        authority,
        path,
    })
}

fn validate_scheme(scheme: &str) -> Result<()> {
    if is_valid_scheme(scheme) {
        Ok(())
    } else {
        bail!("Error: invalid resource scheme: {scheme}")
    }
}

fn is_valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '+' | '.' | '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyHandler(&'static str);

    impl ResourceHandler for DummyHandler {
        fn scheme(&self) -> &'static str {
            self.0
        }

        fn resolve(&self, _req: &ResourceRequest, _ctx: &ToolContext) -> Result<Resource> {
            unreachable!()
        }
    }

    #[test]
    fn registered_scheme_is_routed() {
        let mut router = ResourceRouter::default();
        router
            .register(Arc::new(DummyHandler("kb")), false)
            .unwrap();
        assert!(router.can_handle("kb://policy/rust"));
    }

    #[test]
    fn ordinary_path_is_not_url_like() {
        let router = ResourceRouter::default();
        assert!(!router.is_url_like("src/foo.rs"));
    }

    #[test]
    fn windows_drive_path_is_not_url_like() {
        let router = ResourceRouter::default();
        assert!(!router.is_url_like("C:\\foo"));
    }

    #[test]
    fn duplicate_scheme_registration_fails() {
        let mut router = ResourceRouter::default();
        router
            .register(Arc::new(DummyHandler("kb")), false)
            .unwrap();
        assert!(
            router
                .register(Arc::new(DummyHandler("kb")), false)
                .is_err()
        );
    }

    #[test]
    fn built_in_scheme_cannot_be_replaced_for_tests() {
        let mut router = ResourceRouter::default().with_builtin(DummyHandler("kb"));
        assert!(
            router
                .replace_handler_for_tests(Arc::new(DummyHandler("kb")))
                .is_err()
        );
    }
}
