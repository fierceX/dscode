use super::*;

#[tokio::test]
async fn cancel_wakes_cancelled() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());
    token.cancel();
    assert!(token.is_cancelled());
    token.cancelled().await; // should return immediately
}

#[tokio::test]
async fn linked_child_cancel_does_not_cancel_parent() {
    let parent = CancellationToken::new();
    let child = parent.linked_child_token();
    child.cancel();
    assert!(child.is_cancelled());
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn linked_child_observes_parent_cancel() {
    let parent = CancellationToken::new();
    let child = parent.linked_child_token();
    parent.cancel();
    child.cancelled().await;
    assert!(child.is_cancelled());
}
