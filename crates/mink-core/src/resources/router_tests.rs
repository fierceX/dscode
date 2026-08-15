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
