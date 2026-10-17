#[path = "support/cache.rs"]
mod cache;
#[test]
fn authorization_cache_updates_affect_vm_verification() {
    cache::shared_cache(false);
}
