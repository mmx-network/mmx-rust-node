#[path = "support/cache.rs"]
mod cache;
#[test]
fn vm_cache_updates_affect_contract_authorization() {
    cache::shared_cache(true);
}
